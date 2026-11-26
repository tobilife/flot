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

mod attempts;
mod candidates;
mod execute;
mod reuse;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{OnceLock, RwLock};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::core::attest::VouchedStep;
use crate::core::flow::{Flow, Step};
use crate::core::metrics::{self, Metrics, Spend};
use crate::core::receipt::{
    candidate_rank, sum_costs, verdict_for, ClaimCounts, DepRef, FileDigest, Receipt,
    ReceiptIdentity, RunAttempt, RunCandidate, RunManifest, RunStep, StepStatus, FLOT_VERSION,
};
use crate::engine::baseline::Baseline;
use crate::engine::claims::ProofOutcome;
use crate::engine::exec;
use crate::engine::keying::{self, Facts};
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
    /// Who vouched for each step reused from an imported run. The runner
    /// leaves it empty: the service judges the signers the manifest records
    /// against the store's attestations and trust list, as `show` does.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub vouched: Vec<VouchedStep>,
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
    /// The signers a reused receipt from an imported run rests on.
    vouched_by: Vec<String>,
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
    /// Each step's receipt in the newest run of this flow recorded here,
    /// read once when a step first needs it.
    latest: OnceLock<HashMap<String, String>>,
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
            latest: OnceLock::new(),
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
            if step.always {
                limitations.push(
                    "runs on every run (always); the steps that need it are reused only when its receipt comes out the same, which running it decides"
                        .into(),
                );
            }
            let force = options.force || drift.is_some() || step.always;
            // A dependency with no known receipt makes the downstream key
            // unpredictable until the dependency actually runs.
            let (step_key, reusable) = match keying::dep_refs(step, &receipts) {
                Some(deps) if !step.candidates.is_empty() => {
                    let reusable = if force {
                        None
                    } else {
                        self.best_cached_candidate(step, &deps, &pending, &mut limitations)
                            .map(|recalled| recalled.receipt)
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
                            .map(|recalled| recalled.receipt)
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
        // Held to no revision, a proof can be edited along with the work it
        // judges; what it read last time here is the one thing to compare.
        let proofs_before = match self.baseline {
            Some(_) => HashMap::new(),
            None => {
                // Only a step with a proof has one to compare, and the store
                // is read until each such step is found.
                let proven: Vec<String> = order
                    .iter()
                    .filter(|step_id| {
                        self.flow
                            .step(step_id)
                            .is_some_and(|step| step.verify.is_some())
                    })
                    .cloned()
                    .collect();
                self.proofs_before(&proven)
            }
        };

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
            if let Some((run_id, Some(earlier))) = proofs_before.get(step_id) {
                let changed = proof_changes(earlier, &receipt.identity);
                if !changed.is_empty() {
                    step_limitations.push(format!(
                        "its proof changed since run {run_id} ({}); read the change before trusting the verdict",
                        changed.join(", ")
                    ));
                }
            }
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
                vouched_by: entry.vouched_by.clone(),
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
        } else if order.iter().any(|step_id| {
            self.flow
                .step(step_id)
                .is_some_and(|step| step.verify.is_some())
        }) {
            limitations.push(
                "proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)"
                    .into(),
            );
        }
        // A selection that leaves steps out is recorded, so no report reads
        // its verdict as the flow's.
        let partial =
            (order.len() < self.flow.steps.len()).then_some(crate::core::receipt::Partial {
                selected: order.len(),
                of: self.flow.steps.len(),
            });
        if let Some(partial) = partial {
            limitations.push(format!(
                "partial run: {} of {} steps selected",
                partial.selected, partial.of
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
            partial,
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
            vouched: Vec::new(),
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
        let mut vouched_by = Vec::new();

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
        } else if let Some((known, restored)) = if force || step.always {
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
            vouched_by = known.vouched_by;
            (known.receipt, true)
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
        // attempt must not displace a success recorded earlier. A reused
        // import is remembered with its signers again: remembered as this
        // host's work, it would outlive their trust and read as local work.
        if receipt.is_reusable() {
            self.store
                .remember(&receipt.identity.step_key, &receipt.receipt_id, &vouched_by)?;
        }
        Ok(Settled {
            receipt,
            cached,
            vouched_by,
            notes,
            attempts,
            candidates,
        })
    }

    /// For each of `steps`, the newest run executed here that has it, and its
    /// receipt there (`None` when that receipt is unreadable). Imported runs
    /// are not this host's, and a run of another flow is not this step's.
    fn proofs_before(
        &self,
        steps: &[String],
    ) -> HashMap<String, (String, Option<ReceiptIdentity>)> {
        let mut found = HashMap::new();
        for run_id in self.store.list_runs().unwrap_or_default() {
            if found.len() == steps.len() {
                break;
            }
            if self.store.is_imported(&run_id) {
                continue;
            }
            let Ok(run) = self.store.resolve_run(&run_id) else {
                continue;
            };
            if run.flow != self.flow.meta.name {
                continue;
            }
            for entry in run.steps {
                if found.contains_key(&entry.step_id) || !steps.contains(&entry.step_id) {
                    continue;
                }
                let earlier = self
                    .store
                    .load_receipt(&entry.receipt_id)
                    .ok()
                    .map(|receipt| receipt.identity);
                found.insert(entry.step_id, (run_id.clone(), earlier));
            }
        }
        found
    }
}

/// What differs between the proofs two receipts of a step record: the files
/// it read, its command and its claims. A command neither receipt records is
/// not compared.
fn proof_changes(earlier: &ReceiptIdentity, now: &ReceiptIdentity) -> Vec<String> {
    let mut changed = keying::changed_paths(&earlier.verify_inputs, &now.verify_inputs);
    let command = |identity: &ReceiptIdentity| {
        identity
            .claims
            .iter()
            .find_map(|claim| claim.verify_command.clone())
    };
    if let (Some(before), Some(after)) = (command(earlier), command(now)) {
        if before != after {
            changed.push("the verify command".into());
        }
    }
    if !earlier
        .claims
        .iter()
        .map(|claim| &claim.text)
        .eq(now.claims.iter().map(|claim| &claim.text))
    {
        changed.push("the claims".into());
    }
    changed
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
