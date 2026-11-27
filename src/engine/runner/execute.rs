//! Executing a step once: its environment, its command and proof, and the
//! receipt that records them, or that says why it never ran.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

use crate::core::digest;
use crate::core::flow::{ResolvedBudget, Sandbox, Step};
use crate::core::metrics::{self, Metrics};
use crate::core::preview::stream_preview;
use crate::core::receipt::{
    ClaimRecord, ClaimStatus, FileDigest, Observed, Receipt, ReceiptIdentity, SandboxRecord,
    StepStatus, ToolIdentity,
};
use crate::engine::claims::{self, Hold, Proof, ProofOutcome, Red};
use crate::engine::exec;
use crate::engine::keying::{self, Facts};
use crate::engine::sandbox;
use crate::store::Builds;

use super::{Keyed, RunOptions, Runner, METRICS_SEQUENCE};

impl Runner<'_> {
    fn step_env(&self, step: &Step) -> BTreeMap<String, String> {
        let mut env = step.env.clone();
        env.insert("FLOT_FLOW".into(), self.flow.meta.name.clone());
        env.insert("FLOT_STEP_ID".into(), step.id.clone());
        if self.candidate.is_some() {
            // The copy has no `.git` and sits inside the store: stop git
            // from finding the real repository above it, so a candidate's
            // git commands cannot touch the workspace it was copied from.
            if let Some(parent) = self.root.parent() {
                env.insert(
                    "GIT_CEILING_DIRECTORIES".into(),
                    parent.to_string_lossy().into_owned(),
                );
            }
        }
        env
    }

    /// The receipt of a step that never produced a result of its own:
    /// cancelled before it started, or failed because it could not run.
    /// Its limitations are its facts' and then `why`.
    pub(super) fn unrun_receipt(
        &self,
        unrun: Unrun,
        keyed: Keyed<'_>,
        why: String,
    ) -> Result<Receipt> {
        let (status, reason) = match unrun {
            Unrun::Cancelled => (StepStatus::Cancelled, "step was not run"),
            Unrun::Failed => (StepStatus::Failed, "step could not run"),
            Unrun::Unconfined => (StepStatus::Failed, "step could not run in its sandbox"),
        };
        let Keyed {
            step,
            step_key,
            facts,
            deps,
        } = keyed;
        let mut limitations = facts.limitations;
        limitations.push(why);
        let empty = self.store.put_bytes(b"")?;
        Ok(Receipt::seal(
            ReceiptIdentity {
                flow: self.flow.meta.name.clone(),
                step_id: step.id.clone(),
                step_key,
                command: step.run.clone(),
                status,
                exit_code: None,
                stdout_digest: empty.clone(),
                stderr_digest: empty,
                inputs: facts.inputs,
                verify_inputs: facts.verify_inputs,
                outputs: Vec::new(),
                changed: BTreeMap::new(),
                deps,
                claims: claims::not_run(step, reason),
                limitations,
                tool: ToolIdentity::default(),
                candidate: self.candidate.clone(),
                sandbox: None,
            },
            Observed {
                started_at_ms: exec::now_ms(),
                host_os: std::env::consts::OS.to_string(),
                ..Observed::default()
            },
        ))
    }

    /// The receipt of a step whose sandbox cannot be enforced here: a step
    /// that asked to be confined runs confined or not at all.
    fn unconfined(&self, keyed: Keyed<'_>, refused: Unenforceable) -> Result<Receipt> {
        let Unenforceable(error) = refused;
        let why = format!("could not run: its sandbox cannot be enforced here: {error:#}");
        self.unrun_receipt(Unrun::Unconfined, keyed, why)
    }

    /// Run a step once and settle its claims. `since` is the workspace before
    /// the step's first attempt, taken here when this is it.
    ///
    /// The phases, in order: set up where the command runs, run it, judge
    /// how it ended, keep what it produced, hold it to its `may_change`,
    /// bind what it changed, prove its claims, and seal the receipt. Each
    /// adds its limitations after those found before: their order is part
    /// of the receipt id.
    pub(super) fn execute(
        &self,
        keyed: Keyed<'_>,
        extra: &BTreeMap<String, String>,
        options: &RunOptions,
        since: &mut Option<Vec<FileDigest>>,
    ) -> Result<Receipt> {
        let step = keyed.step;
        let budget = self.flow.resolved_budget(step);
        let setup = match self.set_up(step, extra)? {
            Ok(setup) => setup,
            Err(refused) => return self.unconfined(keyed, refused),
        };
        let mut ran = match self.run_step(step, &setup, budget, options, since)? {
            Ok(ran) => ran,
            Err(refused) => return self.unconfined(keyed, refused),
        };
        let mut limitations = keyed.facts.limitations.clone();
        limitations.append(&mut ran.problems);
        let reported = read_metrics(&setup.metrics_path);
        let mut found = Findings::ended(limitations, step, budget, &ran.outcome, reported);
        let produced = self.keep_outputs(step, budget, &ran.outcome, &mut found)?;
        found.strayed(step, ran.strayed.as_deref());
        let after = self.facts(step);
        self.bind_changes(&keyed, &after, &mut ran.changed, &mut found.limitations);
        self.settle_claims(&keyed, &after, &produced.files, budget, options, &mut found);
        Ok(self.seal(keyed, setup, ran, produced, found))
    }

    /// Where the step's command runs and how: its environment, the file it
    /// reports its spend in, and its sandbox, which a step that asked for
    /// one runs in or not at all.
    fn set_up<'s>(
        &self,
        step: &'s Step,
        extra: &BTreeMap<String, String>,
    ) -> Result<Result<Setup<'s>, Unenforceable>> {
        let (mut env, mut metrics_path) = self.metered_env(step)?;
        env.extend(extra.clone());
        // A confined step writes its metrics and temporary files in a scratch
        // directory of its own, not where other steps' work lies.
        let scratch = match step
            .sandbox
            .as_ref()
            .and_then(|policy| policy.write.as_ref())
        {
            None => None,
            Some(_) => {
                let scratch = Scratch::new(self.store.root(), "sandbox")?;
                metrics_path = scratch.path.join("metrics.json");
                env.insert(
                    "FLOT_METRICS".into(),
                    metrics_path.to_string_lossy().into_owned(),
                );
                env.insert(
                    "TMPDIR".into(),
                    scratch.path.join("tmp").to_string_lossy().into_owned(),
                );
                Some(scratch)
            }
        };
        let confined = match &step.sandbox {
            None => None,
            Some(policy) => {
                let own: Vec<PathBuf> =
                    scratch.iter().map(|scratch| scratch.path.clone()).collect();
                let prepared = policy
                    .write
                    .as_deref()
                    .map(|write| sandbox::resolve(&self.root, write))
                    .transpose()
                    .and_then(|write| sandbox::prepare(policy.network, write.as_deref(), &own));
                match prepared {
                    Ok(prepared) => Some((policy, prepared)),
                    Err(error) => return Ok(Err(Unenforceable(error))),
                }
            }
        };
        Ok(Ok(Setup {
            env,
            metrics_path,
            _scratch: scratch,
            confined,
        }))
    }

    /// Run the step's command: alone when it declares `may_change`, so the
    /// files that changed while it ran are its own doing, and between two
    /// snapshots of the workspace when what it changes matters. Each line it
    /// prints is reported as it prints it.
    fn run_step(
        &self,
        step: &Step,
        setup: &Setup<'_>,
        budget: ResolvedBudget,
        options: &RunOptions,
        since: &mut Option<Vec<FileDigest>>,
    ) -> Result<Result<Ran, Unenforceable>> {
        let scoped = !step.may_change.is_empty();
        // A poisoned lock guards nothing: the data is `()`.
        let (_shared, _alone) = if scoped {
            let alone = self
                .scope
                .write()
                .unwrap_or_else(|poison| poison.into_inner());
            (None, Some(alone))
        } else {
            let shared = self
                .scope
                .read()
                .unwrap_or_else(|poison| poison.into_inner());
            (Some(shared), None)
        };
        let binds = self.binds();
        let before =
            (scoped || binds).then(|| keying::snapshot(&self.root, &self.origin, &self.digests));
        let lines = options.progress.lines(&step.id, false);
        let outcome = match exec::run_confined(
            &step.run,
            &self.root,
            &setup.env,
            exec::Limits::of(budget),
            &options.stop,
            setup.confined.as_ref().map(|(_, prepared)| prepared),
            lines.as_ref(),
        ) {
            Ok(outcome) => outcome,
            // Confinement is applied as the step starts, so a kernel that
            // refuses it shows here.
            Err(error) if setup.confined.is_some() => return Ok(Err(Unenforceable(error))),
            Err(error) => {
                return Err(error).with_context(|| format!("step `{}` could not start", step.id));
            }
        };
        let after = before
            .as_ref()
            .map(|_| keying::snapshot(&self.root, &self.origin, &self.digests));
        let mut problems = Vec::new();
        let strayed = match (&before, &after) {
            (Some(before), Some(after)) if scoped => {
                problems.extend(before.problems.iter().cloned());
                problems.extend(after.problems.iter().cloned());
                Some(keying::out_of_scope(step, &before.files, &after.files))
            }
            _ => None,
        };
        drop((_shared, _alone));
        let changed = match (before, after) {
            (Some(before), Some(after)) if binds => {
                keying::changes(since.get_or_insert(before.files), &after.files)
            }
            _ => BTreeMap::new(),
        };
        Ok(Ok(Ran {
            outcome,
            problems,
            strayed,
            changed,
        }))
    }

    /// Whether a receipt binds what its step changed. A candidate's changes
    /// other than its outputs stay in its copy: there is nothing to bind.
    fn binds(&self) -> bool {
        self.candidate.is_none()
    }

    /// Keep whatever the step produced, even when it failed: partial output
    /// is evidence of how it failed. A success that is missing a declared
    /// output did not do what the flow says it does.
    fn keep_outputs(
        &self,
        step: &Step,
        budget: ResolvedBudget,
        outcome: &exec::Outcome,
        found: &mut Findings,
    ) -> Result<Produced> {
        let stdout_digest = self.store.put_bytes(&outcome.stdout)?;
        let stderr_digest = self.store.put_bytes(&outcome.stderr)?;
        let scan = match keying::scan_outputs(&self.root, &step.outputs, |bytes| {
            self.store.put_bytes(bytes)
        }) {
            Ok(scan) => scan,
            Err(error) => {
                found
                    .limitations
                    .push(format!("outputs could not be recorded: {error:#}"));
                keying::Scan {
                    unmatched: step.outputs.clone(),
                    ..keying::Scan::default()
                }
            }
        };
        found.limitations.extend(scan.problems);
        found.missing_outputs(step, &scan.unmatched);
        let (stdout_inline, stdout_truncated) =
            stream_preview(&outcome.stdout, budget.max_inline_bytes);
        let (stderr_inline, stderr_truncated) =
            stream_preview(&outcome.stderr, budget.max_inline_bytes);
        Ok(Produced {
            stdout_digest,
            stderr_digest,
            stdout_inline,
            stdout_truncated,
            stderr_inline,
            stderr_truncated,
            files: scan.files,
        })
    }

    /// Re-read as `after` what the step declared it reads, and bind to its
    /// receipt the files it answers for besides its outputs. A step that
    /// edits its own inputs leaves a receipt describing content that no
    /// longer exists; one that edits its proof cannot have that proof vouch
    /// for it.
    fn bind_changes(
        &self,
        keyed: &Keyed<'_>,
        after: &Facts,
        changed: &mut BTreeMap<String, Option<String>>,
        limitations: &mut Vec<String>,
    ) {
        let (step, facts) = (keyed.step, &keyed.facts);
        let edited_inputs = keying::changed_paths(&facts.inputs, &after.inputs);
        if !edited_inputs.is_empty() {
            limitations.push(format!(
                "the step changed its own declared inputs ({}); this receipt describes them as they were before it ran",
                edited_inputs.join(", ")
            ));
        }
        if !self.binds() {
            return;
        }
        // Declared files the scan does not reach, such as an input git
        // ignores, are bound too.
        for (path, content) in keying::changes(&facts.inputs, &after.inputs)
            .into_iter()
            .chain(keying::changes(&facts.verify_inputs, &after.verify_inputs))
        {
            changed.entry(path).or_insert(content);
        }
        for (path, content) in self.still_in_place(step, &keyed.step_key) {
            changed.entry(path).or_insert(content);
        }
        // Outputs are recorded, restored and checked as outputs.
        changed.retain(|path, _| !step.declares_output(path));
    }

    /// Settle the step's claims on what its proof shows, and for a proof
    /// that must fail before the change, on what it does there. A process
    /// that left its group meanwhile leaves them unverified.
    fn settle_claims(
        &self,
        keyed: &Keyed<'_>,
        after: &Facts,
        files: &[FileDigest],
        budget: ResolvedBudget,
        options: &RunOptions,
        found: &mut Findings,
    ) {
        let step = keyed.step;
        let proof = self.proof(keyed, after, files, budget, options, found);
        let mut claims = claims::settle(
            step,
            found.status,
            found.tolerated,
            proof,
            &mut found.limitations,
        );
        if step.fails_before
            && claims
                .iter()
                .any(|claim| claim.status == ClaimStatus::Verified)
        {
            let (red, red_metrics, red_notes) = self.red(step, budget, options, &mut found.escaped);
            found.spent(red_metrics, red_notes);
            claims = claims::hold_before(claims, red, &mut found.limitations);
        }
        if !step.held.is_empty()
            && claims
                .iter()
                .any(|claim| claim.status == ClaimStatus::Verified)
        {
            let (hold, hold_metrics, hold_notes) =
                self.held(step, budget, options, &mut found.escaped);
            found.spent(hold_metrics, hold_notes);
            claims = claims::hold_held(&step.id, claims, hold, &mut found.limitations);
        }
        found.claims = claims;
        found.escapes();
    }

    /// What the step's proof shows: run when the step succeeded with claims
    /// to prove, against the proof inputs as they were before the step ran
    /// and the outputs as recorded, and only if those are still what it
    /// judged once it is done.
    fn proof(
        &self,
        keyed: &Keyed<'_>,
        after: &Facts,
        files: &[FileDigest],
        budget: ResolvedBudget,
        options: &RunOptions,
        found: &mut Findings,
    ) -> ProofOutcome {
        let (step, facts) = (keyed.step, &keyed.facts);
        let drift = self.drift(step, &facts.verify_inputs);
        let command = match (
            drift,
            claims::proof_command(step, found.status, found.tolerated),
        ) {
            (Some(drift), _) => return drift,
            (None, None) => return ProofOutcome::NotRun,
            (None, Some(command)) => command,
        };
        let changed = keying::changed_paths(&facts.verify_inputs, &after.verify_inputs);
        if !changed.is_empty() {
            return ProofOutcome::Compromised { changed };
        }
        match self.prove(
            step,
            command,
            Place::Work,
            budget,
            options,
            &mut found.escaped,
        ) {
            Err(error) => ProofOutcome::Failed(format!("{error:#}")),
            Ok((proof, proof_metrics, proof_notes)) => {
                found.spent(proof_metrics, proof_notes);
                // A verdict holds only for the files it judged: the outputs
                // as recorded and the proof inputs as they were before it ran.
                let mut changed = self.changed_outputs(step, files);
                changed.extend(keying::changed_paths(
                    &after.verify_inputs,
                    &self.facts(step).verify_inputs,
                ));
                if changed.is_empty() {
                    ProofOutcome::Ran(proof)
                } else {
                    changed.sort();
                    changed.dedup();
                    ProofOutcome::Disturbed { changed }
                }
            }
        }
    }

    /// The receipt of the execution, as its phases found it. Confined in a
    /// scratch directory, the step is done with it once this is sealed.
    fn seal(
        &self,
        keyed: Keyed<'_>,
        setup: Setup<'_>,
        ran: Ran,
        produced: Produced,
        found: Findings,
    ) -> Receipt {
        let Keyed {
            step,
            step_key,
            facts,
            deps,
        } = keyed;
        Receipt::seal(
            ReceiptIdentity {
                flow: self.flow.meta.name.clone(),
                step_id: step.id.clone(),
                step_key,
                command: step.run.clone(),
                status: found.status,
                exit_code: ran.outcome.exit_code,
                stdout_digest: produced.stdout_digest,
                stderr_digest: produced.stderr_digest,
                inputs: facts.inputs,
                verify_inputs: facts.verify_inputs,
                outputs: produced.files,
                changed: ran.changed,
                deps,
                claims: found.claims,
                limitations: found.limitations,
                tool: ToolIdentity::default(),
                candidate: self.candidate.clone(),
                sandbox: setup.confined.map(|(policy, prepared)| SandboxRecord {
                    network: policy.network,
                    write: policy.write.clone(),
                    mechanism: prepared.mechanism,
                }),
            },
            Observed {
                started_at_ms: ran.outcome.started_at_ms,
                duration_ms: ran.outcome.duration_ms,
                host_os: std::env::consts::OS.to_string(),
                stdout_inline: produced.stdout_inline,
                stdout_truncated: produced.stdout_truncated,
                stderr_inline: produced.stderr_inline,
                stderr_truncated: produced.stderr_truncated,
                stdout_bytes: ran.outcome.stdout.len(),
                stderr_bytes: ran.outcome.stderr.len(),
                metrics: found.metrics,
                metrics_notes: found.metrics_notes,
            },
        )
    }

    /// Recorded outputs that now differ from what was recorded, or that are
    /// gone, and files matching an output pattern that appeared since.
    fn changed_outputs(&self, step: &Step, recorded: &[FileDigest]) -> Vec<String> {
        let now = match keying::scan_globs(
            &self.root,
            &step.outputs,
            "output",
            keying::Reach::Everything,
            |bytes| Ok::<_, std::convert::Infallible>(digest::bytes(bytes)),
        ) {
            Ok(scan) => scan.files,
            Err(never) => match never {},
        };
        keying::changed_paths(recorded, &now)
    }

    /// The step's environment plus a fresh `$FLOT_METRICS` path for one
    /// command to report its spend in.
    fn metered_env(&self, step: &Step) -> Result<(BTreeMap<String, String>, PathBuf)> {
        let mut env = self.step_env(step);
        let metrics_path = self.store.root().join("tmp").join(format!(
            "metrics-{}-{}.json",
            std::process::id(),
            METRICS_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(metrics_path.parent().expect("tmp has a parent"))?;
        let _ = std::fs::remove_file(&metrics_path);
        env.insert(
            "FLOT_METRICS".into(),
            metrics_path.to_string_lossy().to_string(),
        );
        Ok((env, metrics_path))
    }

    /// What `step`'s proof does before the change: it runs on a copy of the
    /// work, where it must pass, then again there with every change since
    /// the baseline undone. Both runs count as the step's spend.
    fn red(
        &self,
        step: &Step,
        budget: ResolvedBudget,
        options: &RunOptions,
        escaped: &mut Vec<exec::Escaped>,
    ) -> (Red, Option<Metrics>, Vec<String>) {
        let Some(baseline) = self.baseline else {
            return (Red::NoRevision, None, Vec::new());
        };
        let command = step
            .verify
            .as_deref()
            .expect("a step with fails_before has a verify command");
        let mut spent: Option<Metrics> = None;
        let mut notes = Vec::new();
        let mut prove = |place: Place| -> Result<Proof> {
            let (proof, metrics, proof_notes) =
                self.prove(step, command, place, budget, options, escaped)?;
            spent = match (spent.take(), metrics) {
                (Some(spent), Some(more)) => Some(spent.plus(more)),
                (spent, more) => spent.or(more),
            };
            notes.extend(
                proof_notes
                    .into_iter()
                    .map(|note| format!("before the change, {note}")),
            );
            Ok(proof)
        };
        let red = (|| -> Result<Red> {
            let builds = self.store.builds(&|| options.stop.is_set())?;
            let copy = self.copy(step, &builds, "before")?;
            let green = prove(Place::Copy(&copy))?;
            if green.timed_out || green.exit_code != Some(0) {
                return Ok(Red::CopyFails {
                    exit_code: green.exit_code,
                    timed_out: green.timed_out,
                });
            }
            baseline
                .undo(copy.dir())
                .context("cannot undo the changes in the copy")?;
            Ok(Red::Ran {
                baseline: baseline.label(),
                commit: baseline.commit.clone(),
                proof: prove(Place::Copy(&copy))?,
            })
        })()
        .unwrap_or_else(|error| Red::Unchecked(format!("{error:#}")));
        (red, spent, notes)
    }

    /// What `step`'s proof does with its `held` files as the revision has
    /// them: nothing to run when they are here as they are there, else it
    /// runs on a copy of the work with them put back. That run counts as the
    /// step's spend.
    fn held(
        &self,
        step: &Step,
        budget: ResolvedBudget,
        options: &RunOptions,
        escaped: &mut Vec<exec::Escaped>,
    ) -> (Hold, Option<Metrics>, Vec<String>) {
        let Some(baseline) = self.baseline else {
            return (Hold::NoRevision, None, Vec::new());
        };
        let command = step
            .verify
            .as_deref()
            .expect("a step with held files has a verify command");
        let base = baseline.held_base(&step.held);
        let here = keying::held_files(&self.root, &step.held, &self.digests);
        let files = match baseline.held_changes(&step.held, &here) {
            Ok(files) => files,
            Err(error) => return (Hold::Unchecked(format!("{error:#}")), None, Vec::new()),
        };
        if files.is_empty() {
            return (Hold::Same { base }, None, Vec::new());
        }
        let ran = (|| -> Result<(Proof, Option<Metrics>, Vec<String>)> {
            let builds = self.store.builds(&|| options.stop.is_set())?;
            let copy = self.copy(step, &builds, "held")?;
            baseline
                .hold(copy.dir(), &files)
                .context("cannot put the held files back in the copy")?;
            self.prove(step, command, Place::Copy(&copy), budget, options, escaped)
        })();
        match ran {
            Ok((proof, metrics, notes)) => (
                Hold::Ran {
                    baseline: baseline.label(),
                    base,
                    files,
                    proof,
                },
                metrics,
                notes
                    .into_iter()
                    .map(|note| format!("with the held files as the revision has them, {note}"))
                    .collect(),
            ),
            Err(error) => (Hold::Unchecked(format!("{error:#}")), None, Vec::new()),
        }
    }

    /// A copy of the work for `step`'s proof, in a directory named for what
    /// it is for, `kind`. It is made while `builds` is held, so every file in
    /// it is newer than anything built there before, and a build that goes by
    /// times builds it whole.
    fn copy<'b>(&self, step: &Step, builds: &'b Builds, kind: &str) -> Result<WorkCopy<'b>> {
        let scratch = Scratch::new(self.store.root(), kind)?;
        let copy = WorkCopy {
            dir: scratch.path.join("work"),
            builds,
            _scratch: scratch,
        };
        keying::copy_workspace(&self.root, copy.dir(), step).context("cannot copy the work")?;
        Ok(copy)
    }

    /// Run a proof command in `place`, the flow root or a copy of it, and
    /// keep its combined output as evidence. A proof can spend too, an agent
    /// asked to review for instance, so it reports through its own
    /// `$FLOT_METRICS`, counted as the step's spend. Processes it started
    /// that left its process group are added to `escaped`. Each line it
    /// prints is reported as the step's proof's.
    fn prove(
        &self,
        step: &Step,
        command: &str,
        place: Place,
        budget: ResolvedBudget,
        options: &RunOptions,
        escaped: &mut Vec<exec::Escaped>,
    ) -> Result<(Proof, Option<Metrics>, Vec<String>)> {
        let (mut env, metrics_path) = self.metered_env(step)?;
        let place = match place {
            Place::Work => self.root.as_path(),
            Place::Copy(copy) => {
                // A copy has no `.git` and sits inside the store: git must
                // not find the real repository above it.
                if let Some(parent) = copy.dir().parent() {
                    env.insert(
                        "GIT_CEILING_DIRECTORIES".into(),
                        parent.to_string_lossy().into_owned(),
                    );
                }
                // Nor may cargo build it where the work builds, as a shared
                // CARGO_TARGET_DIR would have it: cargo goes by its sources'
                // times and paths relative to the package, so the work's
                // next build would take the copy's binaries for its own.
                // Every copy builds in one place, held by one at a time, and
                // without what only a debugger or the next edit of the same
                // files would use, unless the step asks for it: debug info
                // is most of a build's size, and a copy is never edited.
                let builds = copy.builds.dir().to_string_lossy().into_owned();
                for name in ["CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR"] {
                    env.insert(name.into(), builds.clone());
                }
                for name in [
                    "CARGO_PROFILE_DEV_DEBUG",
                    "CARGO_PROFILE_TEST_DEBUG",
                    "CARGO_INCREMENTAL",
                ] {
                    env.entry(name.into()).or_insert_with(|| "0".into());
                }
                copy.dir()
            }
        };
        let lines = options.progress.lines(&step.id, true);
        let verification = exec::run_confined(
            command,
            place,
            &env,
            exec::Limits::of(budget),
            &options.stop,
            None,
            lines.as_ref(),
        )
        .with_context(|| format!("verify command for step `{}` could not start", step.id))?;
        escaped.extend(verification.escaped.iter().cloned());
        let (metrics, notes) = read_metrics(&metrics_path);
        let notes = notes
            .into_iter()
            .map(|note| format!("verify command: {note}"))
            .collect();
        let mut combined = verification.stdout;
        combined.extend_from_slice(&verification.stderr);
        Ok((
            Proof {
                exit_code: if verification.interrupted || verification.output_overflow {
                    None
                } else {
                    verification.exit_code
                },
                timed_out: verification.timed_out,
                output_digest: self.store.put_bytes(&combined)?,
            },
            metrics,
            notes,
        ))
    }
}

/// How a step that never produced a result of its own ended.
pub(super) enum Unrun {
    /// The run did not start it.
    Cancelled,
    /// It could not run.
    Failed,
    /// Its sandbox cannot be enforced here.
    Unconfined,
}

/// Where and how a step's command runs.
struct Setup<'s> {
    env: BTreeMap<String, String>,
    /// Where the command reports what it spent.
    metrics_path: PathBuf,
    /// The confined step's own directory for its metrics and temporary
    /// files, removed with the setup.
    _scratch: Option<Scratch>,
    /// The sandbox the step asked for, prepared.
    confined: Option<(&'s Sandbox, sandbox::Prepared)>,
}

/// Why a step's sandbox cannot be enforced here.
struct Unenforceable(anyhow::Error);

/// What running a step's command left: how it ended, and what it changed.
struct Ran {
    outcome: exec::Outcome,
    /// What the snapshots around a step with `may_change` could not read.
    problems: Vec<String>,
    /// Files it changed outside its `may_change`, when it declares one.
    strayed: Option<Vec<String>>,
    /// What every attempt so far changed, when the receipt binds that.
    changed: BTreeMap<String, Option<String>>,
}

/// What a step produced, kept in the store: its output streams, with what
/// its receipt shows of them inline, and the files of its declared outputs.
struct Produced {
    stdout_digest: String,
    stderr_digest: String,
    stdout_inline: String,
    stdout_truncated: bool,
    stderr_inline: String,
    stderr_truncated: bool,
    files: Vec<FileDigest>,
}

/// What the phases after a step's command find: its status, its claims,
/// and its limitations in the order found, which is part of the receipt
/// id; and what the step and its proofs spent.
struct Findings {
    status: StepStatus,
    /// A failure that `allow_failure` let pass: the step is `ok`, and
    /// proves nothing.
    tolerated: bool,
    claims: Vec<ClaimRecord>,
    limitations: Vec<String>,
    metrics: Option<Metrics>,
    metrics_notes: Vec<String>,
    /// Processes that left their group, from the step and from its proofs.
    escaped: Vec<exec::Escaped>,
}

impl Findings {
    /// The step's status from how its command ended: a limitation, after
    /// `limitations`, for each way it did not simply exit zero, and whether
    /// `allow_failure` let a failure pass. `reported` is what it reported
    /// spending.
    fn ended(
        mut limitations: Vec<String>,
        step: &Step,
        budget: ResolvedBudget,
        outcome: &exec::Outcome,
        reported: (Option<Metrics>, Vec<String>),
    ) -> Findings {
        if outcome.strays_stopped {
            limitations.push(
                "left processes running when it exited; they were stopped, so nothing a step starts outlives it".into(),
            );
        }
        if outcome.output_detached {
            limitations.push(
                "a process that left the step's process group still held its output; captured output may be incomplete".into(),
            );
        }
        let mut tolerated = false;
        let status = if outcome.timed_out {
            limitations.push(format!(
                "stopped at its {}s timeout budget; output is partial",
                budget.timeout_secs
            ));
            StepStatus::Timeout
        } else if outcome.interrupted {
            limitations.push("stopped because flot was asked to stop; output is partial".into());
            StepStatus::Failed
        } else if outcome.output_overflow {
            limitations.push(format!(
                "stopped after {} bytes on one output stream, its max_output_bytes budget; output is cut there",
                budget.max_output_bytes
            ));
            StepStatus::Failed
        } else if outcome.exit_code == Some(0) {
            StepStatus::Ok
        } else if step.allow_failure {
            tolerated = true;
            limitations.push(format!(
                "{} but the step is declared allow_failure, so the run is not failed by it",
                ending(outcome)
            ));
            StepStatus::Ok
        } else {
            if let Some(signal) = outcome.signal {
                limitations.push(format!("was killed by signal {signal}"));
            }
            StepStatus::Failed
        };
        let (metrics, metrics_notes) = reported;
        Findings {
            status,
            tolerated,
            claims: Vec::new(),
            limitations,
            metrics,
            metrics_notes,
            escaped: outcome.escaped.clone(),
        }
    }

    /// A success missing a declared output fails, unless the step is
    /// declared `allow_failure`.
    fn missing_outputs(&mut self, step: &Step, unmatched: &[String]) {
        if !self.status.is_ok() || unmatched.is_empty() {
            return;
        }
        let missing = unmatched.join(", ");
        if step.allow_failure {
            self.tolerated = true;
            self.limitations.push(format!(
                "produced no file for declared output {missing}, but the step is declared allow_failure"
            ));
        } else {
            self.limitations
                .push(format!("produced no file for declared output {missing}"));
            self.status = StepStatus::Failed;
        }
    }

    /// A step that changed files outside its `may_change` fails, unless it
    /// is declared `allow_failure`.
    fn strayed(&mut self, step: &Step, strayed: Option<&[String]>) {
        let Some(strayed) = strayed.filter(|strayed| !strayed.is_empty()) else {
            return;
        };
        let list = strayed.join(", ");
        if step.allow_failure {
            self.tolerated = self.tolerated || self.status.is_ok();
            self.limitations.push(format!(
                "changed files outside may_change ({list}), but the step is declared allow_failure"
            ));
        } else {
            self.limitations
                .push(format!("changed files outside may_change: {list}"));
            if self.status.is_ok() {
                self.status = StepStatus::Failed;
            }
        }
    }

    /// Count what a proof of the step reported spending as the step's.
    fn spent(&mut self, metrics: Option<Metrics>, notes: Vec<String>) {
        self.metrics = match (self.metrics, metrics) {
            (Some(step), Some(proof)) => Some(step.plus(proof)),
            (step, proof) => step.or(proof),
        };
        self.metrics_notes.extend(notes);
    }

    /// A process that left its group may still have been at work while the
    /// proof ran, or after: whatever the proof said is not evidence.
    fn escapes(&mut self) {
        if self.escaped.is_empty() {
            return;
        }
        let pids = |traced: bool| -> Option<String> {
            let mut pids: Vec<u32> = self
                .escaped
                .iter()
                .filter(|escapee| escapee.traced == traced)
                .map(|escapee| escapee.pid)
                .collect();
            pids.sort_unstable();
            pids.dedup();
            let named: Vec<String> = pids.iter().map(u32::to_string).collect();
            (!named.is_empty()).then(|| {
                format!(
                    "{} {}",
                    if named.len() == 1 { "pid" } else { "pids" },
                    named.join(", ")
                )
            })
        };
        if let Some(pids) = pids(true) {
            self.limitations.push(format!(
                "a process it started left its process group and was stopped ({pids})"
            ));
        }
        if let Some(pids) = pids(false) {
            self.limitations.push(format!(
                "a process that left its process group was stopped while this step ran ({pids}); nothing tells which step started it, so each step running then answers for it"
            ));
        }
        // A refutation stands: the step is not let off by what escaped.
        for claim in &mut self.claims {
            if claim.status == ClaimStatus::Verified {
                claim.status = ClaimStatus::Unverified;
                claim.reason = Some("a process left its process group while the step ran".into());
            }
        }
    }
}

/// How a command that did not exit zero ended, for a limitation.
fn ending(outcome: &exec::Outcome) -> String {
    match (outcome.exit_code, outcome.signal) {
        (Some(code), _) => format!("exited {code}"),
        (None, Some(signal)) => format!("was killed by signal {signal}"),
        (None, None) => "ended without an exit status".into(),
    }
}

/// Largest metrics file read. The file is a handful of numbers; anything
/// bigger is not one.
const MAX_METRICS_BYTES: u64 = 64 * 1024;

/// Read and remove one step's metrics file. Absence is not an error: most steps
/// do not bill anything. Anything else that is not a small, regular metrics
/// file is reported and treated as silent: the path is the step's to write, so
/// it may hold a pipe or a link to a device that never ends.
fn read_metrics(path: &Path) -> (Option<Metrics>, Vec<String>) {
    let ignored = |why: String| {
        let _ = std::fs::remove_file(path);
        (
            None,
            vec![format!(
                "metrics file ignored: {why}; this step's spend is unknown"
            )],
        )
    };
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (None, Vec::new()),
        Err(error) => return ignored(format!("cannot inspect it ({error})")),
    };
    if !meta.file_type().is_file() {
        return ignored("it is not a regular file".into());
    }
    if meta.len() > MAX_METRICS_BYTES {
        return ignored(format!("it is larger than {MAX_METRICS_BYTES} bytes"));
    }
    let text = match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return ignored("it is not UTF-8".into()),
        },
        Err(error) => return ignored(format!("cannot read it ({error})")),
    };
    let _ = std::fs::remove_file(path);
    match metrics::parse(&text) {
        Ok((metrics, notes)) => (Some(metrics), notes),
        Err(error) => (
            None,
            vec![format!(
                "metrics file ignored: {error:#}; this step's spend is unknown"
            )],
        ),
    }
}

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Where a proof runs.
enum Place<'a> {
    /// In the work itself.
    Work,
    /// In a copy of it.
    Copy(&'a WorkCopy<'a>),
}

/// A copy of the work, which exists only while the build its proofs use is
/// held, and is removed when it is done with.
struct WorkCopy<'b> {
    dir: PathBuf,
    builds: &'b Builds,
    _scratch: Scratch,
}

impl WorkCopy<'_> {
    fn dir(&self) -> &Path {
        &self.dir
    }
}

/// A directory in the store's `tmp` for one confined execution or one copy
/// of the work, removed when it is done with.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// A directory named for what it holds, `kind`.
    fn new(store_root: &Path, kind: &str) -> Result<Self> {
        let path = store_root.join("tmp").join(format!(
            "{kind}-{}-{}",
            std::process::id(),
            SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("tmp"))
            .with_context(|| format!("cannot create {}", path.display()))?;
        Ok(Scratch { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
