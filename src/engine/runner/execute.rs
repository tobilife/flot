//! Executing a step once: its environment, its command and proof, and the
//! receipt that records them, or that says why it never ran.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::core::digest;
use crate::core::flow::Step;
use crate::core::metrics::{self, Metrics};
use crate::core::preview::stream_preview;
use crate::core::receipt::{
    ClaimStatus, DepRef, FileDigest, Observed, Receipt, ReceiptIdentity, SandboxRecord, StepStatus,
    ToolIdentity,
};
use crate::engine::claims::{self, Proof, ProofOutcome, Red};
use crate::engine::exec;
use crate::engine::keying::{self, Facts};
use crate::engine::sandbox;

use super::{Runner, METRICS_SEQUENCE};

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
    #[allow(clippy::too_many_arguments)]
    pub(super) fn unrun_receipt(
        &self,
        status: StepStatus,
        reason: &str,
        step: &Step,
        step_key: &str,
        facts: Facts,
        deps: Vec<DepRef>,
        limitations: Vec<String>,
    ) -> Result<Receipt> {
        let empty = self.store.put_bytes(b"")?;
        Ok(Receipt::seal(
            ReceiptIdentity {
                flow: self.flow.meta.name.clone(),
                step_id: step.id.clone(),
                step_key: step_key.to_string(),
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

    /// Run a step once and settle its claims. `since` is the workspace before
    /// the step's first attempt, taken here when this is it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn execute(
        &self,
        step: &Step,
        step_key: &str,
        facts: Facts,
        deps: Vec<DepRef>,
        extra: &BTreeMap<String, String>,
        stop: &exec::StopSignal,
        since: &mut Option<Vec<FileDigest>>,
    ) -> Result<Receipt> {
        let mut limitations = facts.limitations.clone();
        let budget = self.flow.resolved_budget(step);
        let (mut env, mut metrics_path) = self.metered_env(step)?;
        env.extend(extra.clone());
        let timeout = Duration::from_secs(budget.timeout_secs);
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
        // A step that asked to be confined runs confined or not at all.
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
                    Err(error) => {
                        limitations.push(format!(
                            "could not run: its sandbox cannot be enforced here: {error:#}"
                        ));
                        return self.unrun_receipt(
                            StepStatus::Failed,
                            "step could not run in its sandbox",
                            step,
                            step_key,
                            facts,
                            deps,
                            limitations,
                        );
                    }
                }
            }
        };
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
        // What a step changes is bound to its receipt. A candidate's changes
        // other than its outputs stay in its copy: there is nothing to bind.
        let binds = self.candidate.is_none();
        let before = (scoped || binds).then(|| keying::snapshot(&self.root, &self.origin));
        let outcome = match exec::run_confined(
            &step.run,
            &self.root,
            &env,
            timeout,
            budget.max_output_bytes,
            stop,
            confined.as_ref().map(|(_, prepared)| prepared),
        ) {
            Ok(outcome) => outcome,
            // Confinement is applied as the step starts, so a kernel that
            // refuses it shows here.
            Err(error) if confined.is_some() => {
                limitations.push(format!(
                    "could not run: its sandbox cannot be enforced here: {error:#}"
                ));
                return self.unrun_receipt(
                    StepStatus::Failed,
                    "step could not run in its sandbox",
                    step,
                    step_key,
                    facts,
                    deps,
                    limitations,
                );
            }
            Err(error) => {
                return Err(error).with_context(|| format!("step `{}` could not start", step.id));
            }
        };
        let after = before
            .as_ref()
            .map(|_| keying::snapshot(&self.root, &self.origin));
        let strayed = match (&before, &after) {
            (Some(before), Some(after)) if scoped => {
                limitations.extend(before.problems.iter().cloned());
                limitations.extend(after.problems.iter().cloned());
                Some(keying::out_of_scope(step, &before.files, &after.files))
            }
            _ => None,
        };
        drop((_shared, _alone));
        let mut changed = match (before, after) {
            (Some(before), Some(after)) if binds => {
                keying::changes(since.get_or_insert(before.files), &after.files)
            }
            _ => BTreeMap::new(),
        };
        let (mut step_metrics, mut metrics_notes) = read_metrics(&metrics_path);
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
        // Processes that left their group, from the step and from its proofs.
        let mut escaped = outcome.escaped.clone();

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
                ending(&outcome)
            ));
            StepStatus::Ok
        } else {
            if let Some(signal) = outcome.signal {
                limitations.push(format!("was killed by signal {signal}"));
            }
            StepStatus::Failed
        };

        let stdout_digest = self.store.put_bytes(&outcome.stdout)?;
        let stderr_digest = self.store.put_bytes(&outcome.stderr)?;

        // Keep whatever the step produced, even when it failed: partial output
        // is evidence of how it failed. A success that is missing a declared
        // output did not do what the flow says it does.
        let produced = match keying::scan_outputs(&self.root, &step.outputs, |bytes| {
            self.store.put_bytes(bytes)
        }) {
            Ok(produced) => produced,
            Err(error) => {
                limitations.push(format!("outputs could not be recorded: {error:#}"));
                keying::Scan {
                    unmatched: step.outputs.clone(),
                    ..keying::Scan::default()
                }
            }
        };
        limitations.extend(produced.problems.iter().cloned());
        let status = if status.is_ok() && !produced.unmatched.is_empty() {
            let missing = produced.unmatched.join(", ");
            if step.allow_failure {
                tolerated = true;
                limitations.push(format!(
                    "produced no file for declared output {missing}, but the step is declared allow_failure"
                ));
                status
            } else {
                limitations.push(format!("produced no file for declared output {missing}"));
                StepStatus::Failed
            }
        } else {
            status
        };
        let status = match strayed {
            Some(strayed) if !strayed.is_empty() => {
                let list = strayed.join(", ");
                if step.allow_failure {
                    tolerated = tolerated || status.is_ok();
                    limitations.push(format!(
                        "changed files outside may_change ({list}), but the step is declared allow_failure"
                    ));
                    status
                } else {
                    limitations.push(format!("changed files outside may_change: {list}"));
                    if status.is_ok() {
                        StepStatus::Failed
                    } else {
                        status
                    }
                }
            }
            _ => status,
        };

        // Re-read what the step declared it reads. A step that edits its own
        // inputs leaves a receipt describing content that no longer exists; one
        // that edits its proof cannot have that proof vouch for it.
        let after = self.facts(step);
        let edited_inputs = keying::changed_paths(&facts.inputs, &after.inputs);
        if !edited_inputs.is_empty() {
            limitations.push(format!(
                "the step changed its own declared inputs ({}); this receipt describes them as they were before it ran",
                edited_inputs.join(", ")
            ));
        }
        if binds {
            // Declared files the scan does not reach, such as an input git
            // ignores, are bound too.
            for (path, content) in keying::changes(&facts.inputs, &after.inputs)
                .into_iter()
                .chain(keying::changes(&facts.verify_inputs, &after.verify_inputs))
            {
                changed.entry(path).or_insert(content);
            }
            for (path, content) in self.still_in_place(step, step_key) {
                changed.entry(path).or_insert(content);
            }
            // Outputs are recorded, restored and checked as outputs.
            changed.retain(|path, _| !step.declares_output(path));
        }
        let drift = self.drift(step, &facts.verify_inputs);
        let proof = match (drift, claims::proof_command(step, status, tolerated)) {
            (Some(drift), _) => drift,
            (None, None) => ProofOutcome::NotRun,
            (None, Some(command)) => {
                let changed = keying::changed_paths(&facts.verify_inputs, &after.verify_inputs);
                if !changed.is_empty() {
                    ProofOutcome::Compromised { changed }
                } else {
                    match self.prove(
                        step,
                        command,
                        &self.root,
                        timeout,
                        budget.max_output_bytes,
                        stop,
                        &mut escaped,
                    ) {
                        Err(error) => ProofOutcome::Failed(format!("{error:#}")),
                        Ok((proof, proof_metrics, proof_notes)) => {
                            step_metrics = match (step_metrics, proof_metrics) {
                                (Some(step), Some(proof)) => Some(step.plus(proof)),
                                (step, proof) => step.or(proof),
                            };
                            metrics_notes.extend(proof_notes);
                            // A verdict holds only for the files it judged:
                            // the outputs as recorded and the proof inputs as
                            // they were before it ran.
                            let mut changed = self.changed_outputs(step, &produced.files);
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
            }
        };
        let mut claims = claims::settle(step, status, tolerated, proof, &mut limitations);
        if step.fails_before
            && claims
                .iter()
                .any(|claim| claim.status == ClaimStatus::Verified)
        {
            let (red, red_metrics, red_notes) =
                self.red(step, timeout, budget.max_output_bytes, stop, &mut escaped);
            step_metrics = match (step_metrics, red_metrics) {
                (Some(step), Some(red)) => Some(step.plus(red)),
                (step, red) => step.or(red),
            };
            metrics_notes.extend(red_notes);
            claims = claims::hold_before(claims, red, &mut limitations);
        }
        if !escaped.is_empty() {
            // It may still have been at work while the proof ran, or after:
            // whatever the proof said is not evidence.
            let pids = |traced: bool| -> Option<String> {
                let mut pids: Vec<u32> = escaped
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
                limitations.push(format!(
                    "a process it started left its process group and was stopped ({pids})"
                ));
            }
            if let Some(pids) = pids(false) {
                limitations.push(format!(
                    "a process that left its process group was stopped while this step ran ({pids}); nothing tells which step started it, so each step running then answers for it"
                ));
            }
            // A refutation stands: the step is not let off by what escaped.
            for claim in &mut claims {
                if claim.status == ClaimStatus::Verified {
                    claim.status = ClaimStatus::Unverified;
                    claim.reason =
                        Some("a process left its process group while the step ran".into());
                }
            }
        }

        let (stdout_inline, stdout_truncated) =
            stream_preview(&outcome.stdout, budget.max_inline_bytes);
        let (stderr_inline, stderr_truncated) =
            stream_preview(&outcome.stderr, budget.max_inline_bytes);

        Ok(Receipt::seal(
            ReceiptIdentity {
                flow: self.flow.meta.name.clone(),
                step_id: step.id.clone(),
                step_key: step_key.to_string(),
                command: step.run.clone(),
                status,
                exit_code: outcome.exit_code,
                stdout_digest,
                stderr_digest,
                inputs: facts.inputs,
                verify_inputs: facts.verify_inputs,
                outputs: produced.files,
                changed,
                deps,
                claims,
                limitations,
                tool: ToolIdentity::default(),
                candidate: self.candidate.clone(),
                sandbox: confined.map(|(policy, prepared)| SandboxRecord {
                    network: policy.network,
                    write: policy.write.clone(),
                    mechanism: prepared.mechanism,
                }),
            },
            Observed {
                started_at_ms: outcome.started_at_ms,
                duration_ms: outcome.duration_ms,
                host_os: std::env::consts::OS.to_string(),
                stdout_inline,
                stdout_truncated,
                stderr_inline,
                stderr_truncated,
                stdout_bytes: outcome.stdout.len(),
                stderr_bytes: outcome.stderr.len(),
                metrics: step_metrics,
                metrics_notes,
            },
        ))
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
        timeout: Duration,
        max_output_bytes: usize,
        stop: &exec::StopSignal,
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
        let mut prove = |place: &Path| -> Result<Proof> {
            let (proof, metrics, proof_notes) = self.prove(
                step,
                command,
                place,
                timeout,
                max_output_bytes,
                stop,
                escaped,
            )?;
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
            let scratch = Scratch::new(self.store.root(), "before")?;
            let copy = scratch.path.join("work");
            keying::copy_workspace(&self.root, &copy, step).context("cannot copy the work")?;
            let green = prove(&copy)?;
            if green.timed_out || green.exit_code != Some(0) {
                return Ok(Red::CopyFails {
                    exit_code: green.exit_code,
                    timed_out: green.timed_out,
                });
            }
            baseline
                .undo(&copy)
                .context("cannot undo the changes in the copy")?;
            Ok(Red::Ran {
                baseline: baseline.label(),
                commit: baseline.commit.clone(),
                proof: prove(&copy)?,
            })
        })()
        .unwrap_or_else(|error| Red::Unchecked(format!("{error:#}")));
        (red, spent, notes)
    }

    /// Run a proof command in `place`, the flow root or a copy of it, and
    /// keep its combined output as evidence. A proof can spend too, an agent
    /// asked to review for instance, so it reports through its own
    /// `$FLOT_METRICS`, counted as the step's spend. Processes it started
    /// that left its process group are added to `escaped`.
    #[allow(clippy::too_many_arguments)]
    fn prove(
        &self,
        step: &Step,
        command: &str,
        place: &Path,
        timeout: Duration,
        max_output_bytes: usize,
        stop: &exec::StopSignal,
        escaped: &mut Vec<exec::Escaped>,
    ) -> Result<(Proof, Option<Metrics>, Vec<String>)> {
        let (mut env, metrics_path) = self.metered_env(step)?;
        if place != self.root {
            // A copy has no `.git` and sits inside the store: git must not
            // find the real repository above it.
            if let Some(parent) = place.parent() {
                env.insert(
                    "GIT_CEILING_DIRECTORIES".into(),
                    parent.to_string_lossy().into_owned(),
                );
            }
        }
        let verification = exec::run_command(command, place, &env, timeout, max_output_bytes, stop)
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
