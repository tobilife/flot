//! The runner: turn a flow into receipts.
//!
//! It composes the engine's parts and owns no policy of its own:
//! `keying` decides identity, `claims` decides what was proven, `scheduler`
//! decides when a step may start. What lives here is the sequencing between
//! them and the process execution that produces the facts they judge.
//!
//! A dependency that did not reach `ok` cancels its dependents before they
//! cause side effects; a reused receipt is only ever a settled success, and
//! reusing it puts back the files it recorded.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::core::digest;
use crate::core::flow::{Flow, Step};
use crate::core::metrics::{self, Metrics, Spend};
use crate::core::receipt::{
    candidate_rank, sum_costs, verdict_for, ClaimCounts, ClaimStatus, DepRef, FileDigest, Observed,
    Receipt, ReceiptIdentity, RunAttempt, RunCandidate, RunManifest, RunStep, SandboxRecord,
    StepStatus, ToolIdentity, FLOT_VERSION,
};
use crate::engine::baseline::Baseline;
use crate::engine::claims::{self, Proof, ProofOutcome, Red};
use crate::engine::exec;
use crate::engine::keying::{self, Facts};
use crate::engine::sandbox;
use crate::engine::scheduler;
use crate::store::Store;

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Run only this step and its transitive dependencies.
    pub only: Option<String>,
    /// Ignore the receipt cache and re-run every selected step.
    pub force: bool,
    /// How many steps may run at once. Overrides the flow's `max_parallel`.
    pub jobs: Option<usize>,
    /// Run cost ceiling in dollars. Overrides the flow's `max_cost_usd`.
    pub max_cost_usd: Option<f64>,
    /// Git revision proofs must match. Overrides the flow's `proofs_from`.
    pub proofs_from: Option<String>,
    /// Withdraws the run: running steps stop, the rest are cancelled, and the
    /// run is still recorded.
    pub stop: exec::StopSignal,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepReport {
    pub step_id: String,
    pub receipt_id: String,
    /// Digest of the complete stdout, for `flot cat` when the preview is cut.
    pub stdout_digest: String,
    pub status: StepStatus,
    pub cached: bool,
    pub claims: ClaimCounts,
    pub duration_ms: u64,
    pub stdout_inline: String,
    pub stdout_truncated: bool,
    pub stderr_inline: String,
    pub stderr_truncated: bool,
    pub limitations: Vec<String>,
    pub metrics: Option<Metrics>,
    /// Files the step produced, as recorded in its receipt.
    pub outputs: Vec<FileDigest>,
    /// How many times this run executed the step: 0 when it was reused or
    /// not run, more than 1 when earlier attempts did not hold.
    pub attempts: usize,
    /// The candidate whose receipt this is, when the step names candidates.
    pub candidate: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub manifest: RunManifest,
    pub steps: Vec<StepReport>,
}

/// Predicted execution plan without side effects.
#[derive(Debug, Clone, Serialize)]
pub struct PlanEntry {
    pub step_id: String,
    pub step_key: Option<String>,
    pub cached: bool,
    pub needs: Vec<String>,
    pub claims: usize,
    pub verified_by: Option<String>,
    pub inputs: usize,
    pub outputs: Vec<String>,
    pub limitations: Vec<String>,
}

/// Unique metrics paths for concurrently executing steps.
static METRICS_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Spend known at the moment a step is about to execute.
#[derive(Debug, Clone, Copy)]
struct CostGate {
    ceiling_micro_usd: u64,
    spent_micro_usd: u64,
}

/// Everything a worker needs to settle one step, computed from its settled
/// dependencies on the scheduling thread.
struct Work<'f> {
    step: &'f Step,
    blocked: Vec<String>,
    deps: Vec<DepRef>,
    gate: Option<CostGate>,
}

/// A step that reached a receipt, and whether that receipt was reused.
struct Settled {
    receipt: Receipt,
    cached: bool,
    /// What reusing the receipt changed in the workspace. Not part of the
    /// receipt, which describes the step, not this run of it.
    notes: Vec<String>,
    /// Earlier attempts at the step in this run that did not hold, oldest
    /// first; `receipt` is the last attempt's.
    attempts: Vec<Receipt>,
    /// The step's other candidates this run executed, in flow order.
    candidates: Vec<Contender>,
}

/// One candidate of a step as this run executed it.
struct Contender {
    name: String,
    /// Its last attempt's receipt.
    receipt: Receipt,
    /// Its earlier attempts, oldest first.
    attempts: Vec<Receipt>,
}

impl Contender {
    /// What its attempts cost together, `None` when any reported no cost.
    fn cost_micro_usd(&self) -> Option<u64> {
        self.attempts
            .iter()
            .chain(std::iter::once(&self.receipt))
            .try_fold(0u64, |total, receipt| {
                sum_costs(
                    Some(total),
                    receipt
                        .observed
                        .metrics
                        .and_then(|metrics| metrics.cost_micro_usd),
                )
            })
    }
}

impl Settled {
    /// Cost reported by the attempts this run executed.
    fn spent_micro_usd(&self) -> u64 {
        let last = (!self.cached).then_some(&self.receipt);
        self.attempts
            .iter()
            .chain(last)
            .chain(
                self.candidates
                    .iter()
                    .flat_map(|candidate| candidate.attempts.iter().chain([&candidate.receipt])),
            )
            .filter_map(|receipt| receipt.observed.metrics?.cost_micro_usd)
            .fold(0u64, u64::saturating_add)
    }
}

/// Unique scratch workspaces for candidates.
static WORKSPACE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct Runner<'a> {
    flow: &'a Flow,
    root: PathBuf,
    store: &'a Store,
    /// The candidate this runner executes, in a copy of the workspace.
    candidate: Option<String>,
    /// The workspace `root` is a copy of, or `root` itself: its repository
    /// says which files are executable where the file system cannot.
    origin: PathBuf,
    /// The revision proofs must match, when the run checks them against one.
    baseline: Option<&'a Baseline>,
    /// Held shared while a step runs, and alone by a step with `may_change`,
    /// so the files that changed while it ran are its own doing.
    scope: RwLock<()>,
}

impl<'a> Runner<'a> {
    /// What `step` reads in this runner's workspace, keyed as its origin's.
    fn facts(&self, step: &Step) -> keying::Facts {
        if self.root == self.origin {
            keying::facts_of(&self.root, step)
        } else {
            keying::facts_of_copy(&self.root, &self.origin, step)
        }
    }

    pub fn new(flow: &'a Flow, root: &Path, store: &'a Store) -> Self {
        Runner {
            flow,
            root: root.to_path_buf(),
            store,
            candidate: None,
            origin: root.to_path_buf(),
            baseline: None,
            scope: RwLock::new(()),
        }
    }

    /// Check every proof against `baseline` before running it or reusing a
    /// receipt that rests on it.
    pub fn with_baseline(mut self, baseline: Option<&'a Baseline>) -> Self {
        self.baseline = baseline;
        self
    }

    /// Why `step`'s proof does not match the baseline, as it reads now.
    fn drift(&self, step: &Step, verify_inputs: &[FileDigest]) -> Option<ProofOutcome> {
        let baseline = self.baseline?;
        let (reasons, claims) = match baseline.drift(step, verify_inputs) {
            Ok(drift) => {
                let drift = drift?;
                (drift.reasons, drift.claims)
            }
            // Fails closed: a proof that cannot be compared is not anchored.
            Err(error) => (vec![format!("{error:#}")], step.claims.clone()),
        };
        Some(ProofOutcome::Unanchored {
            baseline: baseline.label(),
            reasons,
            claims,
        })
    }

    fn selection(&self, options: &RunOptions) -> Result<Vec<String>> {
        match &options.only {
            Some(step_id) => self.flow.closure(step_id),
            None => self.flow.topo_order(),
        }
    }

    fn step_key(&self, step: &Step, facts: &Facts, deps: &[DepRef]) -> String {
        keying::step_key(&self.flow.meta.name, step, facts, deps)
    }

    /// A reusable receipt for this key, with why one was passed over.
    ///
    /// Reuse writes the receipt's outputs back, so a receipt naming a file this
    /// flow does not declare for the step is never reused. Neither is one whose
    /// outputs were changed since: its claims were proven against the recorded
    /// content, and both overwriting the change and keeping it under that
    /// receipt would leave a verdict about files that are not there. A missing
    /// output is fine; reuse restores it.
    fn recall(
        &self,
        step: &Step,
        step_key: &str,
        notes: &mut Vec<String>,
    ) -> Result<Option<Receipt>> {
        let Some(recalled) = self.store.recall(step_key)? else {
            return Ok(None);
        };
        // An imported receipt stands in for local work only while a signer
        // of its bundle is still trusted: revoking a signer revokes this too.
        if !recalled.vouched_by.is_empty() {
            let trusted = self.store.trusted_keys()?;
            if !recalled
                .vouched_by
                .iter()
                .any(|signer| trusted.contains_key(signer))
            {
                notes.push(
                    "cached receipt came from a signer this store no longer trusts; not reused"
                        .into(),
                );
                return Ok(None);
            }
        }
        let known = recalled.receipt;
        // The key covers the claims; this guards the reuse decision itself.
        let claimed: Vec<&str> = known
            .identity
            .claims
            .iter()
            .map(|claim| claim.text.as_str())
            .collect();
        if claimed != step.claims.iter().map(String::as_str).collect::<Vec<_>>() {
            notes.push(
                "cached receipt settled other claims than this step makes; not reused".into(),
            );
            return Ok(None);
        }
        // Whether the proof fails before the change depends on the revision
        // too, which the key does not cover.
        if step.fails_before {
            let checked_at = known
                .identity
                .claims
                .iter()
                .find_map(|claim| claim.before.as_ref())
                .map(|before| before.commit.as_str());
            if checked_at != self.baseline.map(|baseline| baseline.commit.as_str()) {
                notes.push(
                    "cached receipt was not checked against this revision before the change; the step runs again"
                        .into(),
                );
                return Ok(None);
            }
        }
        for output in &known.identity.outputs {
            if !step.declares_output(&output.path) {
                notes.push(format!(
                    "cached receipt names {}, which this step does not declare as an output; not reused",
                    output.path
                ));
                return Ok(None);
            }
            if !keying::within_root(&self.root, &output.path)? {
                notes.push(format!(
                    "output {} is reached through a symbolic link; the cached receipt is not reused",
                    output.path
                ));
                return Ok(None);
            }
            let path = self.root.join(&output.path);
            match std::fs::read(&path) {
                Ok(bytes)
                    if digest::bytes(&bytes) != output.digest
                        || keying::is_executable(&path) != output.executable =>
                {
                    notes.push(format!(
                        "output {} changed since its receipt; the step runs again",
                        output.path
                    ));
                    return Ok(None);
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("cannot read output {}", output.path))
                }
            }
        }
        Ok(Some(known))
    }

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

    /// Plan the run: what would execute, what would be reused, what evidence
    /// gaps the flow already has. Reads files, runs nothing.
    pub fn plan(&self, options: &RunOptions) -> Result<Vec<PlanEntry>> {
        let order = self.selection(options)?;
        let mut receipts: HashMap<String, String> = HashMap::new();
        // Outputs that reusing a predicted-cached step will put back. A
        // dependent's inputs are digested as they will be once that happened.
        let mut pending: Vec<FileDigest> = Vec::new();
        let mut plan = Vec::new();
        for step_id in order {
            let step = self.flow.step(&step_id).expect("validated step");
            let facts = keying::facts_with(&self.root, step, &pending);
            let mut limitations = facts.limitations.clone();
            let drift = self.drift(step, &facts.verify_inputs);
            if let Some(ProofOutcome::Unanchored {
                baseline, reasons, ..
            }) = &drift
            {
                limitations.push(format!(
                    "the proof differs from {baseline} ({}); its claims would stay unverified",
                    reasons.join(", ")
                ));
            }
            let force = options.force || drift.is_some();
            // A dependency with no known receipt makes the downstream key
            // unpredictable until the dependency actually runs.
            let (step_key, reusable) = match keying::dep_refs(step, &receipts) {
                Some(deps) if !step.candidates.is_empty() => {
                    let reusable = if force {
                        None
                    } else {
                        self.best_cached_candidate(step, &deps, &pending, &mut limitations)
                    };
                    (
                        reusable
                            .as_ref()
                            .map(|receipt| receipt.identity.step_key.clone()),
                        reusable,
                    )
                }
                Some(deps) => {
                    let key = self.step_key(step, &facts, &deps);
                    let reusable = if force {
                        None
                    } else {
                        // As in `run`: a receipt that cannot be checked is a
                        // miss, and the step would run.
                        self.recall(step, &key, &mut limitations)
                            .unwrap_or_else(|error| {
                                limitations.push(format!(
                                    "cached receipt not reused: {error:#}; the step runs again"
                                ));
                                None
                            })
                    };
                    (Some(key), reusable)
                }
                None => (None, None),
            };
            if step.claims.is_empty() {
                limitations.push("step declares no claims; its success proves nothing".into());
            } else if step.verify.is_none() {
                limitations.push("step declares claims without a verify command".into());
            }
            let cached = reusable.is_some();
            if let Some(receipt) = reusable {
                pending.extend(
                    receipt
                        .identity
                        .outputs
                        .iter()
                        .filter(|output| !self.root.join(&output.path).exists())
                        .cloned(),
                );
                receipts.insert(step_id.clone(), receipt.receipt_id);
            }
            plan.push(PlanEntry {
                step_id,
                step_key,
                cached,
                needs: step.needs.clone(),
                claims: step.claims.len(),
                verified_by: step.verify.clone(),
                inputs: facts.inputs.len() + facts.verify_inputs.len(),
                outputs: step.outputs.clone(),
                limitations,
            });
        }
        Ok(plan)
    }

    pub fn run(&self, options: &RunOptions) -> Result<RunReport> {
        if options.jobs == Some(0) {
            bail!("jobs must be at least 1");
        }
        let order = self.selection(options)?;
        let jobs = options.jobs.or(self.flow.meta.max_parallel).unwrap_or(1);
        let ceiling_micro_usd = match options.max_cost_usd.or(self.flow.meta.max_cost_usd) {
            Some(dollars) => {
                Some(metrics::usd_to_micro(dollars).context("the run cost ceiling is invalid")?)
            }
            None => None,
        };
        // Held until the manifest is written: until then nothing names this
        // run's receipts, and gc must not collect them.
        let _lock = self.store.lock(false)?;
        let started_at_ms = exec::now_ms();
        let started = std::time::Instant::now();

        let needs: BTreeMap<String, Vec<String>> = order
            .iter()
            .map(|step_id| {
                let step = self.flow.step(step_id).expect("validated step");
                (step_id.clone(), step.needs.clone())
            })
            .collect();
        let settled = scheduler::run_dag(
            &order,
            &needs,
            jobs,
            |step_id, settled: &HashMap<String, Settled>| {
                self.prepare(step_id, settled, ceiling_micro_usd)
            },
            |work| self.settle(work, options),
        )?;

        let mut manifest_steps: Vec<RunStep> = Vec::new();
        let mut reports: Vec<StepReport> = Vec::new();
        let mut totals = ClaimCounts::default();
        let mut spend = Spend::default();
        // Executions whose cost is unknown: they reported nothing, or no cost.
        let mut silent: Vec<String> = Vec::new();
        let mut unpriced: Vec<String> = Vec::new();
        let mut limitations: Vec<String> = Vec::new();

        // Assemble in dependency order, never in completion order: the run root
        // must not depend on which worker finished first.
        for step_id in &order {
            let entry = settled
                .get(step_id)
                .expect("every selected step settles exactly once");
            let receipt = &entry.receipt;
            let cached = entry.cached;
            let mut step_limitations = receipt.identity.limitations.clone();
            step_limitations.extend(entry.notes.iter().cloned());
            let counts = receipt.claim_counts();
            totals.add(counts);
            let mut attempts = Vec::new();
            for earlier in &entry.attempts {
                spend.add_executed(earlier.observed.metrics);
                unknown_cost(earlier, &mut silent, &mut unpriced);
                attempts.push(RunAttempt {
                    receipt_id: earlier.receipt_id.clone(),
                    observed_digest: self.store.put_observation(&earlier.observed)?,
                });
            }
            let mut candidates = Vec::new();
            for candidate in &entry.candidates {
                let mut attempts = Vec::new();
                for earlier in candidate.attempts.iter().chain([&candidate.receipt]) {
                    spend.add_executed(earlier.observed.metrics);
                    unknown_cost(earlier, &mut silent, &mut unpriced);
                    attempts.push(RunAttempt {
                        receipt_id: earlier.receipt_id.clone(),
                        observed_digest: self.store.put_observation(&earlier.observed)?,
                    });
                }
                let last = attempts.pop().expect("a candidate has a last receipt");
                candidates.push(RunCandidate {
                    name: candidate.name.clone(),
                    receipt_id: last.receipt_id,
                    observed_digest: last.observed_digest,
                    attempts,
                });
            }
            if !cached && receipt.identity.candidate.is_some() {
                let kept = Contender {
                    name: receipt.identity.candidate.clone().unwrap_or_default(),
                    receipt: receipt.clone(),
                    attempts: entry.attempts.clone(),
                };
                let step = self.flow.step(step_id).expect("validated step");
                let told: Vec<String> = step
                    .candidates
                    .iter()
                    .filter_map(|name| {
                        std::iter::once(&kept)
                            .chain(&entry.candidates)
                            .find(|ran| &ran.name == name)
                    })
                    .map(|ran| {
                        let tries = ran.attempts.len() + 1;
                        format!(
                            "{} {}{} ({})",
                            ran.name,
                            outcome(&ran.receipt),
                            if tries > 1 {
                                format!(" after {tries} attempts")
                            } else {
                                String::new()
                            },
                            ran.cost_micro_usd()
                                .map(metrics::format_usd)
                                .unwrap_or_else(|| "cost unknown".into())
                        )
                    })
                    .collect();
                step_limitations.push(format!(
                    "candidates {}; kept {}",
                    told.join(", "),
                    kept.name
                ));
            }
            if !entry.attempts.is_empty() {
                let mut told: Vec<String> = entry
                    .attempts
                    .iter()
                    .enumerate()
                    .map(|(number, earlier)| format!("{} {}", number + 1, outcome(earlier)))
                    .collect();
                told.push(format!("{} {}", told.len() + 1, outcome(receipt)));
                step_limitations.push(format!(
                    "took {} attempts of {} allowed: {}",
                    told.len(),
                    self.flow.step(step_id).expect("validated step").attempts(),
                    told.join(", ")
                ));
            }
            if cached {
                spend.add_reused(receipt.observed.metrics);
            } else if receipt.identity.status != StepStatus::Cancelled {
                spend.add_executed(receipt.observed.metrics);
                unknown_cost(receipt, &mut silent, &mut unpriced);
            }
            for limitation in &step_limitations {
                limitations.push(format!("{}: {}", step_id, limitation));
            }
            if !cached {
                for note in &receipt.observed.metrics_notes {
                    limitations.push(format!("{}: {}", step_id, note));
                }
            }
            manifest_steps.push(RunStep {
                step_id: step_id.clone(),
                receipt_id: receipt.receipt_id.clone(),
                status: receipt.identity.status,
                cached,
                claims: counts,
                observed_digest: self.store.put_observation(&receipt.observed)?,
                attempts,
                candidates,
            });
            reports.push(StepReport {
                step_id: step_id.clone(),
                receipt_id: receipt.receipt_id.clone(),
                stdout_digest: receipt.identity.stdout_digest.clone(),
                status: receipt.identity.status,
                cached,
                claims: counts,
                duration_ms: receipt.observed.duration_ms,
                stdout_inline: receipt.observed.stdout_inline.clone(),
                stdout_truncated: receipt.observed.stdout_truncated,
                stderr_inline: receipt.observed.stderr_inline.clone(),
                stderr_truncated: receipt.observed.stderr_truncated,
                limitations: step_limitations,
                metrics: receipt.observed.metrics,
                outputs: receipt.identity.outputs.clone(),
                candidate: receipt.identity.candidate.clone(),
                attempts: if cached || receipt.identity.status == StepStatus::Cancelled {
                    0
                } else {
                    entry.attempts.len() + 1
                },
            });
        }

        if let Some(baseline) = self.baseline {
            limitations.push(format!("proofs checked against {}", baseline.label()));
            let missing = baseline.missing_steps(|id| self.flow.step(id).is_some());
            if !missing.is_empty() {
                limitations.push(format!(
                    "steps with claims at {} are not in this flow: {}; their claims are not made",
                    baseline.label(),
                    missing.join(", ")
                ));
            }
        }
        if order.len() < self.flow.steps.len() {
            limitations.push(format!(
                "partial run: {} of {} steps selected",
                order.len(),
                self.flow.steps.len()
            ));
        }
        if let Some(ceiling) = ceiling_micro_usd {
            if !silent.is_empty() {
                limitations.push(format!(
                    "{} executed step(s) reported no metrics ({}); the cost ceiling cannot see what they spent",
                    silent.len(),
                    silent.join(", ")
                ));
            }
            if !unpriced.is_empty() {
                limitations.push(format!(
                    "{} executed step(s) reported no cost ({}); the cost ceiling cannot see what they spent",
                    unpriced.len(),
                    unpriced.join(", ")
                ));
            }
            if spend.cost_micro_usd > ceiling {
                limitations.push(format!(
                    "spent {} against a {} ceiling: the ceiling is checked before each step starts, so steps already running when it was crossed finished",
                    metrics::format_usd(spend.cost_micro_usd),
                    metrics::format_usd(ceiling)
                ));
            }
        }
        limitations.extend(self.output_conflicts(&order, &settled)?);
        if jobs > 1 {
            limitations.push(format!(
                "up to {jobs} steps ran concurrently; flot checks the outputs steps declare, but cannot see files they write without declaring them"
            ));
        }

        let pairs: Vec<(String, String)> = manifest_steps
            .iter()
            .map(|step| (step.step_id.clone(), step.receipt_id.clone()))
            .collect();
        let root_digest = RunManifest::root_of(&pairs);
        let verdict = verdict_for(&manifest_steps, totals);
        let duration_ms = exec::millis(started.elapsed());
        // What the guard refused since the run before this one finished,
        // agents this run started included.
        let previous_end = self
            .store
            .resolve_run("latest")
            .map(|previous| previous.started_at_ms + previous.duration_ms)
            .unwrap_or(0);
        let refusals = self
            .store
            .refusals(previous_end, started_at_ms + duration_ms)?;
        let manifest = RunManifest {
            run_id: RunManifest::id_for(started_at_ms, &root_digest),
            flow: self.flow.meta.name.clone(),
            flow_digest: self.flow.digest(),
            root_digest,
            verdict,
            steps: manifest_steps,
            claims: totals,
            limitations,
            max_parallel: jobs,
            spend,
            max_cost_micro_usd: ceiling_micro_usd,
            started_at_ms,
            duration_ms,
            flot_version: FLOT_VERSION.to_string(),
            refusals,
        };
        self.store.save_run(&manifest)?;
        Ok(RunReport {
            manifest,
            steps: reports,
        })
    }

    /// Derive a ready step's work from its settled dependencies. Spend is read
    /// here, at launch: steps already running when the ceiling is crossed
    /// finish, and the run reports the overshoot.
    fn prepare(
        &self,
        step_id: &str,
        settled: &HashMap<String, Settled>,
        ceiling_micro_usd: Option<u64>,
    ) -> Work<'a> {
        let step = self.flow.step(step_id).expect("validated step");
        // A dependency blocks when it failed, or when it succeeded but a proof
        // refuted its claims. Work built on refuted work cannot lead to a
        // passing run, so running it would only spend time and money.
        let blocked = step
            .needs
            .iter()
            .filter_map(|need| {
                let receipt = &settled.get(need)?.receipt;
                if !receipt.identity.status.is_ok() {
                    Some(format!("{need} ({})", receipt.identity.status.label()))
                } else if receipt.claim_counts().refuted > 0 {
                    Some(format!("{need} (claims refuted)"))
                } else {
                    None
                }
            })
            .collect();
        let receipt_ids: HashMap<String, String> = settled
            .iter()
            .map(|(id, entry)| (id.clone(), entry.receipt.receipt_id.clone()))
            .collect();
        let deps = keying::dep_refs(step, &receipt_ids)
            .expect("dependencies settle before a step launches");
        let gate = ceiling_micro_usd.map(|ceiling_micro_usd| CostGate {
            ceiling_micro_usd,
            spent_micro_usd: settled.values().fold(0u64, |total, entry| {
                total.saturating_add(entry.spent_micro_usd())
            }),
        });
        Work {
            step,
            blocked,
            deps,
            gate,
        }
    }

    /// Produce one step's receipt: cancelled, reused, or freshly executed.
    ///
    /// A problem with this one step, such as a command that cannot start or an
    /// output that cannot be restored, becomes a receipt saying so, and the
    /// run still records a manifest with everything it spent. Only a store
    /// that cannot record receipts at all stops the run.
    fn settle(&self, work: Work<'_>, options: &RunOptions) -> Result<Settled> {
        let force = options.force;
        let Work {
            step,
            blocked,
            deps,
            gate,
        } = work;
        let facts = self.facts(step);
        let mut limitations = facts.limitations.clone();
        let step_key = self.step_key(step, &facts, &deps);
        // Facts about this run, such as why a cached receipt was passed over,
        // are notes. Only facts about the work go into a receipt.
        let mut notes = Vec::new();
        let mut attempts = Vec::new();
        let mut candidates = Vec::new();

        let (receipt, cached) = if options.stop.is_set() {
            limitations.push("not run: flot was asked to stop".into());
            (
                self.unrun_receipt(
                    StepStatus::Cancelled,
                    "step was not run",
                    step,
                    &step_key,
                    facts,
                    deps,
                    limitations,
                )?,
                false,
            )
        } else if !blocked.is_empty() {
            limitations.push(format!(
                "not run: dependency {} did not hold",
                blocked.join(", ")
            ));
            (
                self.unrun_receipt(
                    StepStatus::Cancelled,
                    "step was not run",
                    step,
                    &step_key,
                    facts,
                    deps,
                    limitations,
                )?,
                false,
            )
        } else if let Some((known, restored)) = if force {
            None
        } else if self.drift(step, &facts.verify_inputs).is_some() {
            // A receipt proven against something else does not stand in.
            None
        } else if step.candidates.is_empty() {
            self.reuse(step, &step_key, &mut notes)
        } else {
            self.reuse_candidate(step, &deps, &mut notes)
        } {
            // Reuse costs nothing, so the ceiling never blocks a cache hit.
            notes.extend(restored);
            (known, true)
        } else if let Some(gate) =
            gate.filter(|gate| gate.spent_micro_usd >= gate.ceiling_micro_usd)
        {
            limitations.push(format!(
                "not run: run cost ceiling of {} reached ({} spent by executed steps)",
                metrics::format_usd(gate.ceiling_micro_usd),
                metrics::format_usd(gate.spent_micro_usd)
            ));
            (
                self.unrun_receipt(
                    StepStatus::Cancelled,
                    "step was not run",
                    step,
                    &step_key,
                    facts,
                    deps,
                    limitations,
                )?,
                false,
            )
        } else if step.candidates.is_empty() {
            let (receipt, earlier) =
                self.attempt(step, step_key, facts, deps, gate, &options.stop, &mut notes)?;
            attempts = earlier;
            (receipt, false)
        } else {
            let mut ran = self.compete(step, &deps, gate, &options.stop, &mut notes)?;
            if ran.is_empty() {
                limitations.push("not run: no candidate started".into());
                (
                    self.unrun_receipt(
                        StepStatus::Cancelled,
                        "step was not run",
                        step,
                        &step_key,
                        facts,
                        deps,
                        limitations,
                    )?,
                    false,
                )
            } else {
                let kept = (0..ran.len())
                    .min_by_key(|&index| {
                        (
                            candidate_rank(&ran[index].receipt, ran[index].cost_micro_usd()),
                            index,
                        )
                    })
                    .expect("at least one candidate ran");
                let kept = ran.remove(kept);
                notes.extend(self.write_back(&kept.receipt));
                attempts = kept.attempts;
                candidates = ran;
                (kept.receipt, false)
            }
        };

        self.store.put_receipt(&receipt)?;
        // Only reusable work may answer for this key. A cancelled or failed
        // attempt must not displace a success recorded earlier.
        if receipt.is_reusable() {
            self.store
                .remember(&receipt.identity.step_key, &receipt.receipt_id, &[])?;
        }
        Ok(Settled {
            receipt,
            cached,
            notes,
            attempts,
            candidates,
        })
    }

    /// The best reusable receipt among a step's candidates, ranked as when
    /// they ran, with its missing outputs put back.
    fn reuse_candidate(
        &self,
        step: &Step,
        deps: &[DepRef],
        notes: &mut Vec<String>,
    ) -> Option<(Receipt, Vec<String>)> {
        let receipt = self.best_cached_candidate(step, deps, &[], notes)?;
        match self.restore_outputs(&receipt) {
            Ok(restored) => Some((receipt, restored)),
            Err(error) => {
                notes.push(format!(
                    "cached receipt not reused: {error:#}; the step runs again"
                ));
                None
            }
        }
    }

    /// Of a step's candidates with a reusable receipt, the one that ranks
    /// best by what it reported when it ran. Writes nothing.
    fn best_cached_candidate(
        &self,
        step: &Step,
        deps: &[DepRef],
        pending: &[FileDigest],
        notes: &mut Vec<String>,
    ) -> Option<Receipt> {
        let mut misses = Vec::new();
        let mut known = Vec::new();
        for name in &step.candidates {
            let candidate = step.as_candidate(name);
            let facts = keying::facts_with(&self.root, &candidate, pending);
            let key = self.step_key(&candidate, &facts, deps);
            match self.recall(&candidate, &key, &mut misses) {
                Ok(Some(receipt)) => known.push(receipt),
                Ok(None) => {}
                Err(error) => misses.push(format!(
                    "cached receipt of candidate {name} not reused: {error:#}"
                )),
            }
        }
        let best = (0..known.len()).min_by_key(|&index| {
            let cost = known[index]
                .observed
                .metrics
                .and_then(|metrics| metrics.cost_micro_usd);
            (candidate_rank(&known[index], cost), index)
        });
        let Some(best) = best else {
            // Another candidate's outputs on disk are expected to refuse a
            // receipt; that is only worth saying when nothing was reused.
            notes.extend(misses);
            return None;
        };
        Some(known.swap_remove(best))
    }

    /// Run each candidate of a step in its own copy of the workspace, in
    /// flow order. A candidate does not start once flot is asked to stop or
    /// the cost ceiling is reached; one whose workspace cannot be prepared
    /// gets a receipt saying so.
    fn compete(
        &self,
        step: &Step,
        deps: &[DepRef],
        gate: Option<CostGate>,
        stop: &exec::StopSignal,
        notes: &mut Vec<String>,
    ) -> Result<Vec<Contender>> {
        let mut ran: Vec<Contender> = Vec::new();
        for name in &step.candidates {
            if stop.is_set() {
                notes.push(format!("candidate {name} not run: flot was asked to stop"));
                continue;
            }
            let gate = gate.map(|gate| CostGate {
                spent_micro_usd: ran.iter().fold(gate.spent_micro_usd, |total, ran| {
                    total.saturating_add(ran.cost_micro_usd().unwrap_or(0))
                }),
                ..gate
            });
            if let Some(gate) = gate.filter(|gate| gate.spent_micro_usd >= gate.ceiling_micro_usd) {
                notes.push(format!(
                    "candidate {name} not run: run cost ceiling of {} reached ({} spent by executed steps)",
                    metrics::format_usd(gate.ceiling_micro_usd),
                    metrics::format_usd(gate.spent_micro_usd)
                ));
                continue;
            }
            let candidate = step.as_candidate(name);
            // Inside the store, so the copy is on the workspace's own
            // filesystem and its files read back the same way.
            let place = self.store.root().join("tmp").join(format!(
                "candidate-{}-{}-{name}",
                std::process::id(),
                WORKSPACE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let runner = Runner {
                flow: self.flow,
                root: place.clone(),
                store: self.store,
                candidate: Some(name.clone()),
                origin: self.root.clone(),
                baseline: self.baseline,
                scope: RwLock::new(()),
            };
            let mut said = Vec::new();
            let (receipt, attempts) = match keying::copy_workspace(&self.root, &place, &candidate) {
                Ok(()) => {
                    let facts = runner.facts(&candidate);
                    let key = runner.step_key(&candidate, &facts, deps);
                    runner.attempt(&candidate, key, facts, deps.to_vec(), gate, stop, &mut said)?
                }
                Err(error) => {
                    let facts = keying::facts_of(&self.root, &candidate);
                    let key = self.step_key(&candidate, &facts, deps);
                    let mut limitations = facts.limitations.clone();
                    limitations.push(format!(
                        "could not run: its workspace could not be prepared: {error:#}"
                    ));
                    let receipt = runner.unrun_receipt(
                        StepStatus::Failed,
                        "step could not run",
                        &candidate,
                        &key,
                        facts,
                        deps.to_vec(),
                        limitations,
                    )?;
                    (receipt, Vec::new())
                }
            };
            let _ = std::fs::remove_dir_all(&place);
            notes.extend(
                said.into_iter()
                    .map(|note| format!("candidate {name}: {note}")),
            );
            self.store.put_receipt(&receipt)?;
            if receipt.is_reusable() {
                self.store
                    .remember(&receipt.identity.step_key, &receipt.receipt_id, &[])?;
            }
            ran.push(Contender {
                name: name.clone(),
                receipt,
                attempts,
            });
        }
        Ok(ran)
    }

    /// Write the kept candidate's outputs into the workspace, replacing what
    /// is there: they are what its claims were proven against.
    fn write_back(&self, receipt: &Receipt) -> Vec<String> {
        let mut notes = Vec::new();
        for output in &receipt.identity.outputs {
            let written = (|| -> Result<()> {
                if !keying::within_root(&self.root, &output.path)? {
                    anyhow::bail!("it is reached through a symbolic link");
                }
                let path = self.root.join(&output.path);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, self.store.read_bytes(&output.digest)?)?;
                #[cfg(unix)]
                if output.executable {
                    use std::os::unix::fs::PermissionsExt;
                    let mut permissions = std::fs::metadata(&path)?.permissions();
                    permissions.set_mode(permissions.mode() | 0o111);
                    std::fs::set_permissions(&path, permissions)?;
                }
                Ok(())
            })();
            if let Err(error) = written {
                notes.push(format!(
                    "output {} of the kept candidate could not be written back: {error:#}",
                    output.path
                ));
            }
        }
        notes
    }

    /// Execute a step until it holds or its attempts run out. An attempt that
    /// did not hold is kept, and the next one is told why in
    /// `$FLOT_FEEDBACK`. A later attempt sees the workspace the earlier one
    /// left, so its facts and key are read again.
    ///
    /// No further attempt starts once flot is asked to stop, once the run's
    /// cost ceiling is reached, or after a command that could not start at
    /// all, which another attempt would not change.
    #[allow(clippy::too_many_arguments)]
    fn attempt(
        &self,
        step: &Step,
        mut step_key: String,
        mut facts: Facts,
        deps: Vec<DepRef>,
        gate: Option<CostGate>,
        stop: &exec::StopSignal,
        notes: &mut Vec<String>,
    ) -> Result<(Receipt, Vec<Receipt>)> {
        let allowed = step.attempts();
        let mut earlier: Vec<Receipt> = Vec::new();
        loop {
            let number = earlier.len() as u32 + 1;
            let mut extra = BTreeMap::new();
            if allowed > 1 {
                extra.insert("FLOT_ATTEMPT".to_string(), number.to_string());
                extra.insert("FLOT_ATTEMPTS".to_string(), allowed.to_string());
            }
            let feedback = match earlier.last() {
                Some(previous) => Some(self.write_feedback(previous, number - 1, allowed)?),
                None => None,
            };
            if let Some(path) = &feedback {
                extra.insert(
                    "FLOT_FEEDBACK".to_string(),
                    path.to_string_lossy().to_string(),
                );
            }
            let executed = self.execute(step, &step_key, facts.clone(), deps.clone(), &extra, stop);
            if let Some(path) = &feedback {
                let _ = std::fs::remove_file(path);
            }
            let receipt = match executed {
                Ok(receipt) => receipt,
                Err(error) => {
                    let mut limitations = facts.limitations.clone();
                    limitations.push(format!("could not run: {error:#}"));
                    let receipt = self.unrun_receipt(
                        StepStatus::Failed,
                        "step could not run",
                        step,
                        &step_key,
                        facts,
                        deps,
                        limitations,
                    )?;
                    return Ok((receipt, earlier));
                }
            };
            if receipt.is_reusable() || number >= allowed || stop.is_set() {
                return Ok((receipt, earlier));
            }
            if let Some(gate) = gate {
                let spent = earlier
                    .iter()
                    .chain(std::iter::once(&receipt))
                    .filter_map(|receipt| receipt.observed.metrics?.cost_micro_usd)
                    .fold(gate.spent_micro_usd, u64::saturating_add);
                if spent >= gate.ceiling_micro_usd {
                    notes.push(format!(
                        "attempt {} of {allowed} not run: run cost ceiling of {} reached ({} spent by executed steps)",
                        number + 1,
                        metrics::format_usd(gate.ceiling_micro_usd),
                        metrics::format_usd(spent)
                    ));
                    return Ok((receipt, earlier));
                }
            }
            self.store.put_receipt(&receipt)?;
            earlier.push(receipt);
            facts = self.facts(step);
            step_key = self.step_key(step, &facts, &deps);
        }
    }

    /// Tell the next attempt why this one did not hold: how it ended, each
    /// claim's verdict, its limitations, and the end of what its proof and
    /// its command printed, all read back from the recorded evidence.
    fn write_feedback(&self, receipt: &Receipt, number: u32, allowed: u32) -> Result<PathBuf> {
        /// Room for each stream, kept from its end, where failures are told.
        const ROOM: usize = 8 * 1024;
        let identity = &receipt.identity;
        let mut text = format!(
            "flot: attempt {number} of {allowed} at step `{}` did not hold.\n\n",
            identity.step_id
        );
        text.push_str(&format!("status: {}", identity.status.label()));
        if let Some(code) = identity.exit_code {
            text.push_str(&format!(" (exit {code})"));
        }
        text.push('\n');
        if !identity.claims.is_empty() {
            text.push_str("claims:\n");
            for claim in &identity.claims {
                text.push_str(&format!("  - {}: {}\n", claim.status.label(), claim.text));
                if let Some(reason) = &claim.reason {
                    text.push_str(&format!("    {reason}\n"));
                }
            }
        }
        if !identity.limitations.is_empty() {
            text.push_str("limitations:\n");
            for limitation in &identity.limitations {
                text.push_str(&format!("  - {limitation}\n"));
            }
        }
        let mut stream = |title: String, digest: &str| -> Result<()> {
            let bytes = self.store.read_bytes(digest)?;
            if bytes.is_empty() {
                return Ok(());
            }
            let (preview, _) = exec::stream_preview(&bytes, ROOM);
            text.push_str(&format!("\n--- {title} ---\n{preview}"));
            if !preview.ends_with('\n') {
                text.push('\n');
            }
            Ok(())
        };
        let mut proofs: Vec<(&str, &str)> = identity
            .claims
            .iter()
            .filter_map(|claim| {
                Some((
                    claim.verify_command.as_deref()?,
                    claim.verify_output_digest.as_deref()?,
                ))
            })
            .collect();
        proofs.dedup();
        for (command, digest) in proofs {
            stream(format!("output of the proof `{command}`"), digest)?;
        }
        stream("stdout".into(), &identity.stdout_digest)?;
        stream("stderr".into(), &identity.stderr_digest)?;
        let path = self.store.root().join("tmp").join(format!(
            "feedback-{}-{}.txt",
            std::process::id(),
            METRICS_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.parent().expect("tmp has a parent"))?;
        std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(path)
    }

    /// A cached receipt with its missing outputs put back, or `None` with the
    /// reason noted when there is none to reuse or it cannot be restored. A
    /// failed restore is a miss, not an error: running the step again is
    /// always a valid way to get its receipt.
    fn reuse(
        &self,
        step: &Step,
        step_key: &str,
        limitations: &mut Vec<String>,
    ) -> Option<(Receipt, Vec<String>)> {
        let known = match self.recall(step, step_key, limitations) {
            Ok(known) => known?,
            Err(error) => {
                limitations.push(format!(
                    "cached receipt not reused: {error:#}; the step runs again"
                ));
                return None;
            }
        };
        match self.restore_outputs(&known) {
            Ok(notes) => Some((known, notes)),
            Err(error) => {
                limitations.push(format!(
                    "cached receipt not reused: {error:#}; the step runs again"
                ));
                None
            }
        }
    }

    /// Put back the outputs of a reused receipt that are missing, so the files
    /// on disk are the ones its claims were proven against. `recall` already
    /// refused any receipt whose outputs exist with other content.
    fn restore_outputs(&self, receipt: &Receipt) -> Result<Vec<String>> {
        let mut notes = Vec::new();
        for output in &receipt.identity.outputs {
            let path = self.root.join(&output.path);
            if path.exists() {
                continue;
            }
            // Checked again here: nothing may be written through a link.
            if !keying::within_root(&self.root, &output.path)? {
                anyhow::bail!(
                    "refusing to restore {}: it is reached through a symbolic link",
                    output.path
                );
            }
            let bytes = self.store.read_bytes(&output.digest)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
            std::fs::write(&path, bytes)
                .with_context(|| format!("cannot restore output {}", output.path))?;
            #[cfg(unix)]
            if output.executable {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&path)?.permissions();
                permissions.set_mode(permissions.mode() | 0o111);
                std::fs::set_permissions(&path, permissions)
                    .with_context(|| format!("cannot make {} executable", output.path))?;
            }
            notes.push(format!(
                "output {} was missing and was restored from its receipt",
                output.path
            ));
        }
        Ok(notes)
    }

    /// Files produced by two steps where neither needs the other. Which content
    /// survives then depends on scheduling, so one receipt describes a file
    /// that is no longer there.
    fn output_conflicts(
        &self,
        order: &[String],
        settled: &HashMap<String, Settled>,
    ) -> Result<Vec<String>> {
        let mut producers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for step_id in order {
            for output in &settled[step_id].receipt.identity.outputs {
                producers
                    .entry(output.path.as_str())
                    .or_default()
                    .push(step_id.as_str());
            }
        }
        let mut conflicts = Vec::new();
        for (path, steps) in producers {
            for (position, first) in steps.iter().enumerate() {
                for second in &steps[position + 1..] {
                    let ordered = self.flow.closure(second)?.iter().any(|id| id == first)
                        || self.flow.closure(first)?.iter().any(|id| id == second);
                    if !ordered {
                        conflicts.push(format!(
                            "steps {first} and {second} both produce {path} and neither needs the other; which content survives depends on scheduling"
                        ));
                    }
                }
            }
        }
        Ok(conflicts)
    }

    /// The receipt of a step that never produced a result of its own:
    /// cancelled before it started, or failed because it could not run.
    #[allow(clippy::too_many_arguments)]
    fn unrun_receipt(
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

    fn execute(
        &self,
        step: &Step,
        step_key: &str,
        facts: Facts,
        deps: Vec<DepRef>,
        extra: &BTreeMap<String, String>,
        stop: &exec::StopSignal,
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
        let before = scoped.then(|| keying::snapshot(&self.root));
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
        let strayed = before.map(|before| {
            let after = keying::snapshot(&self.root);
            limitations.extend(before.problems);
            limitations.extend(after.problems);
            keying::out_of_scope(step, &before.files, &after.files)
        });
        drop((_shared, _alone));
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
                self.red(step, timeout, budget.max_output_bytes, stop);
            step_metrics = match (step_metrics, red_metrics) {
                (Some(step), Some(red)) => Some(step.plus(red)),
                (step, red) => step.or(red),
            };
            metrics_notes.extend(red_notes);
            claims = claims::hold_before(claims, red, &mut limitations);
        }

        let (stdout_inline, stdout_truncated) =
            exec::stream_preview(&outcome.stdout, budget.max_inline_bytes);
        let (stderr_inline, stderr_truncated) =
            exec::stream_preview(&outcome.stderr, budget.max_inline_bytes);

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
            let (proof, metrics, proof_notes) =
                self.prove(step, command, place, timeout, max_output_bytes, stop)?;
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
    /// `$FLOT_METRICS`, counted as the step's spend.
    fn prove(
        &self,
        step: &Step,
        command: &str,
        place: &Path,
        timeout: Duration,
        max_output_bytes: usize,
        stop: &exec::StopSignal,
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

/// How one attempt ended, in a word or two: `held`, or why not.
fn outcome(receipt: &Receipt) -> String {
    let counts = receipt.claim_counts();
    if !receipt.identity.status.is_ok() {
        match receipt.identity.exit_code {
            Some(code) => format!("{} (exit {code})", receipt.identity.status.label()),
            None => receipt.identity.status.label().to_string(),
        }
    } else if counts.refuted > 0 {
        "refuted".into()
    } else if !receipt.is_reusable() {
        "unverified".into()
    } else {
        "held".into()
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

/// Note an execution whose cost is unknown, named by its step and, for a
/// candidate, the candidate.
fn unknown_cost(receipt: &Receipt, silent: &mut Vec<String>, unpriced: &mut Vec<String>) {
    let label = match &receipt.identity.candidate {
        Some(name) => format!("{} ({name})", receipt.identity.step_id),
        None => receipt.identity.step_id.clone(),
    };
    match receipt.observed.metrics {
        None => silent.push(label),
        Some(metrics) if metrics.cost_micro_usd.is_none() => unpriced.push(label),
        Some(_) => {}
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::HostSecret;

    #[test]
    fn zero_jobs_is_refused_before_anything_runs() {
        let dir = tempfile::tempdir().unwrap();
        let flow =
            Flow::parse("[flow]\nname = \"j\"\n\n[[step]]\nid = \"s\"\nrun = \"true\"\n").unwrap();
        let store = Store::open(dir.path(), HostSecret::from_bytes([7; 32])).unwrap();
        let error = Runner::new(&flow, dir.path(), &store)
            .run(&RunOptions {
                jobs: Some(0),
                ..RunOptions::default()
            })
            .unwrap_err();
        assert!(error.to_string().contains("at least 1"), "{error}");
        assert!(store.list_runs().unwrap().is_empty());
    }
}
