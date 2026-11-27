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
mod manifest;
mod reuse;

use self::execute::Unrun;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::core::attest::VouchedStep;
use crate::core::flow::{Flow, Step};
use crate::core::metrics::{self, Metrics};
use crate::core::receipt::{
    candidate_rank, sum_costs, ClaimCounts, DepRef, FileDigest, Receipt, RunManifest, StepStatus,
};
use crate::engine::baseline::Baseline;
use crate::engine::claims::ProofOutcome;
use crate::engine::exec;
use crate::engine::keying::{self, Facts};
use crate::engine::progress::{self, Event};
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
    /// Told what happens as the run goes; by default no one is.
    pub progress: progress::Sink,
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
    /// The `always` steps this step's key, and so its reuse, is predicted on:
    /// the plan takes each to come out as it did last time, which only
    /// running it can tell.
    pub assumes: Vec<String>,
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

/// A step with what keys it: the files it reads, the receipts it follows,
/// and the key they make. Every receipt of the step records them.
#[derive(Clone)]
struct Keyed<'s> {
    step: &'s Step,
    step_key: String,
    facts: Facts,
    deps: Vec<DepRef>,
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
    /// The digests of files already read, shared with candidates' runners.
    digests: Arc<keying::Digests>,
}

impl<'a> Runner<'a> {
    /// What `step` reads in this runner's workspace, keyed as its origin's.
    fn facts(&self, step: &Step) -> keying::Facts {
        if self.root == self.origin {
            keying::facts_of(&self.root, step, &self.digests)
        } else {
            keying::facts_of_copy(&self.root, &self.origin, step, &self.digests)
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
            digests: Arc::default(),
        }
    }

    /// Read a file again only when the file system says it changed since
    /// this runner read it (see [`keying::Digests`]). The fence is stamped in
    /// the store, so `plan`, which writes nothing, reads every file.
    pub fn reusing_digests(mut self) -> Self {
        self.digests = Arc::new(keying::Digests::fenced_in(self.store.root().join("tmp")));
        self
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

    /// `step` with the key `facts` and `deps` make for it.
    fn keyed<'s>(&self, step: &'s Step, facts: Facts, deps: Vec<DepRef>) -> Keyed<'s> {
        Keyed {
            step_key: self.step_key(step, &facts, &deps),
            step,
            facts,
            deps,
        }
    }

    /// Plan the run: what would execute, what would be reused, what evidence
    /// gaps the flow already has. Reads files, runs nothing.
    pub fn plan(&self, options: &RunOptions) -> Result<Vec<PlanEntry>> {
        let order = self.selection(options)?;
        let mut receipts: HashMap<String, String> = HashMap::new();
        // For each step whose receipt is predicted, the `always` steps that
        // prediction rests on.
        let mut assumed: HashMap<String, BTreeSet<String>> = HashMap::new();
        // Outputs that reusing a predicted-cached step will put back. A
        // dependent's inputs are digested as they will be once that happened.
        let mut pending: Vec<FileDigest> = Vec::new();
        let mut plan = Vec::new();
        for step_id in order {
            let step = self.flow.step(&step_id).expect("validated step");
            let facts = keying::facts_with(&self.root, step, &pending, &self.digests);
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
            let assumes: BTreeSet<String> = step
                .needs
                .iter()
                .filter_map(|need| assumed.get(need))
                .flatten()
                .cloned()
                .collect();
            // An `always` step runs, but what it recorded last time for this
            // key is what it comes out as when nothing it probes changed:
            // what follows it is predicted on that.
            let reusable = match (reusable, &step_key) {
                (None, Some(key)) if step.always && !options.force => {
                    self.store.recall(key)?.map(|recalled| recalled.receipt)
                }
                (reusable, _) => reusable,
            };
            if reusable.is_some() {
                let mut passed_on = assumes.clone();
                if step.always {
                    passed_on.insert(step_id.clone());
                }
                assumed.insert(step_id.clone(), passed_on);
            }
            if step.claims.is_empty() {
                limitations.push("step declares no claims; its success proves nothing".into());
            } else if step.verify.is_none() {
                limitations.push("step declares claims without a verify command".into());
            }
            let cached = reusable.is_some() && !step.always;
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
                assumes: assumes.into_iter().collect(),
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
        options.progress.emit(|| Event::RunStarted {
            steps: order.len(),
            jobs,
        });

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
        options.progress.emit(|| Event::RunFinished {
            duration_ms: exec::millis(started.elapsed()),
        });

        let assembly = self.assemble(&order, &settled, jobs, ceiling_micro_usd)?;
        let report = self.report(assembly, started_at_ms, started)?;
        self.store.save_run(&report.manifest)?;
        Ok(report)
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
        let keyed = self.keyed(step, self.facts(step), deps);
        // Facts about this run, such as why a cached receipt was passed over,
        // are notes. Only facts about the work go into a receipt.
        let mut notes = Vec::new();
        let mut attempts = Vec::new();
        let mut candidates = Vec::new();
        let mut vouched_by = Vec::new();

        let (receipt, cached) = if options.stop.is_set() {
            let why = "not run: flot was asked to stop".to_string();
            (self.not_run(keyed, why, options)?, false)
        } else if !blocked.is_empty() {
            let why = format!("not run: dependency {} did not hold", blocked.join(", "));
            (self.not_run(keyed, why, options)?, false)
        } else if let Some((known, restored)) = if force || step.always {
            None
        } else if self.drift(step, &keyed.facts.verify_inputs).is_some() {
            // A receipt proven against something else does not stand in.
            None
        } else if step.candidates.is_empty() {
            self.reuse(step, &keyed.step_key, &mut notes)
        } else {
            self.reuse_candidate(step, &keyed.deps, &mut notes)
        } {
            // Reuse costs nothing, so the ceiling never blocks a cache hit.
            notes.extend(restored);
            vouched_by = known.vouched_by;
            options.progress.emit(|| Event::StepCached {
                step: step.id.clone(),
                candidate: known.receipt.identity.candidate.clone(),
                claims: known.receipt.claim_counts(),
            });
            (known.receipt, true)
        } else if let Some(gate) =
            gate.filter(|gate| gate.spent_micro_usd >= gate.ceiling_micro_usd)
        {
            let why = format!(
                "not run: run cost ceiling of {} reached ({} spent by executed steps)",
                metrics::format_usd(gate.ceiling_micro_usd),
                metrics::format_usd(gate.spent_micro_usd)
            );
            (self.not_run(keyed, why, options)?, false)
        } else if step.candidates.is_empty() {
            let (receipt, earlier) = self.attempt(keyed, gate, options, &mut notes)?;
            attempts = earlier;
            (receipt, false)
        } else {
            let mut ran = self.compete(step, &keyed.deps, gate, options, &mut notes)?;
            if ran.is_empty() {
                let why = "not run: no candidate started".to_string();
                (self.not_run(keyed, why, options)?, false)
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

    /// The receipt of a step the run did not start, for `why`, reported as
    /// finished without executing.
    fn not_run(&self, keyed: Keyed<'_>, why: String, options: &RunOptions) -> Result<Receipt> {
        let receipt = self.unrun_receipt(Unrun::Cancelled, keyed, why)?;
        options
            .progress
            .emit(|| Event::finished(&receipt, 0, Duration::ZERO));
        Ok(receipt)
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

    /// The runner end to end, pinned: which steps run, what each receipt,
    /// manifest and cache entry then holds, byte for byte. Its steps are
    /// shell commands, so it runs where `sh` is the shell.
    #[cfg(unix)]
    mod scenario {
        use super::*;
        use crate::core::receipt::FLOT_VERSION;

        /// A flow that settles a step each way a run can: `build` passes with an
        /// output and a priced metrics file, `lint` fails, `ship` is not run
        /// because `lint` did not hold, `tolerant` fails as `allow_failure`
        /// lets it and collects limitations, `flaky` holds at its second attempt
        /// and `pick` keeps the cheaper of two candidates.
        const SCENARIO: &str = r#"
[flow]
name = "scenario"
max_cost_usd = 1.0

[[step]]
id = "build"
run = '''printf 'built\n' > out.txt; printf '{"cost_usd": 0.25, "input_tokens": 7, "retries": 1}' > "$FLOT_METRICS"; echo building'''
inputs = ["src.txt"]
outputs = ["out.txt"]
claims = ["out.txt says built"]
verify = "sh check.sh"
verify_inputs = ["check.sh"]

[[step]]
id = "lint"
run = "echo 'src.txt: 2 problems' >&2; exit 3"
inputs = ["src.txt"]
claims = ["src.txt is clean"]
verify = "sh lint_check.sh"
verify_inputs = ["lint_check.sh"]

[[step]]
id = "ship"
needs = ["build", "lint"]
run = "cat out.txt"
inputs = ["out.txt"]
claims = ["shipped"]
verify = "true"

[[step]]
id = "tolerant"
run = '''echo trying; printf 'trace\n' > trace.log; printf 'stray\n' > stray.txt; exit 1'''
inputs = ["notes/*.md"]
outputs = ["report.txt"]
may_change = ["trace.log"]
allow_failure = true

[[step]]
id = "flaky"
run = 'test "$FLOT_ATTEMPT" = 2'
inputs = ["src.txt"]
attempts = 2
claims = ["it held"]
verify = "true"

[[step]]
id = "pick"
run = '''printf '%s\n' "$FLOT_CANDIDATE" > pick.txt; if [ "$FLOT_CANDIDATE" = fast ]; then cost=0.01; else cost=0.02; fi; printf '{"cost_usd": %s}' "$cost" > "$FLOT_METRICS"'''
inputs = ["src.txt"]
outputs = ["pick.txt"]
candidates = ["slow", "fast"]
claims = ["pick.txt names a candidate"]
verify = "test -s pick.txt"
"#;

        /// Every byte two runs of `SCENARIO` leave behind: each run's manifest
        /// and step reports, every receipt in the store, every cache entry.
        /// Limitation order is part of every receipt id, so this pins it too.
        #[test]
        fn a_scenario_leaves_exactly_these_bytes() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let write = |path: &str, text: &str| std::fs::write(root.join(path), text).unwrap();
            write("src.txt", "fn main() {}\n");
            write("check.sh", "grep -q built out.txt\n");
            write("lint_check.sh", "exit 0\n");
            let flow = Flow::parse(SCENARIO).unwrap();
            let store = Store::open(root, HostSecret::from_bytes([7; 32])).unwrap();
            let run = || {
                Runner::new(&flow, root, &store)
                    .run(&RunOptions::default())
                    .unwrap()
            };
            let first = run();
            // The second run reuses what held and puts back the output deleted
            // here; `lint` runs again and its proof has changed.
            std::fs::remove_file(root.join("out.txt")).unwrap();
            write("lint_check.sh", "exit 1\n");
            let second = run();

            let record = scenario_record(&flow, &store, &[first, second]);
            for (line, (actual, expected)) in
                record.lines().zip(SCENARIO_RECORD.lines()).enumerate()
            {
                assert_eq!(actual, expected, "line {}", line + 1);
            }
            assert_eq!(record, SCENARIO_RECORD);
        }

        /// A run that reports its progress leaves the same bytes, and tells
        /// each attempt and candidate it executes as started, then finished.
        #[test]
        fn a_run_told_to_a_sink_leaves_the_same_bytes() {
            use std::sync::{Arc, Mutex};

            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let write = |path: &str, text: &str| std::fs::write(root.join(path), text).unwrap();
            write("src.txt", "fn main() {}\n");
            write("check.sh", "grep -q built out.txt\n");
            write("lint_check.sh", "exit 0\n");
            let flow = Flow::parse(SCENARIO).unwrap();
            let store = Store::open(root, HostSecret::from_bytes([7; 32])).unwrap();
            let told = Arc::new(Mutex::new(Vec::new()));
            let kept = Arc::clone(&told);
            let options = RunOptions {
                progress: progress::Sink::new(move |event| {
                    kept.lock().unwrap().push(event.clone())
                }),
                ..RunOptions::default()
            };
            let run = || Runner::new(&flow, root, &store).run(&options).unwrap();
            let first = run();
            std::fs::remove_file(root.join("out.txt")).unwrap();
            write("lint_check.sh", "exit 1\n");
            let second = run();
            assert_eq!(
                scenario_record(&flow, &store, &[first, second]),
                SCENARIO_RECORD
            );

            // One step at a time, so each start is followed by its finish.
            let told = told.lock().unwrap();
            let mut executions = Vec::new();
            let mut open = None;
            for event in told.iter() {
                match event {
                    Event::StepStarted {
                        step,
                        attempt,
                        candidate,
                        ..
                    } => {
                        assert_eq!(open, None, "{event:?}");
                        open = Some((step.clone(), *attempt, candidate.clone()));
                    }
                    Event::StepFinished {
                        step,
                        attempt: 1..,
                        candidate,
                        ..
                    } => {
                        let started = open.take().expect("a finish follows a start");
                        assert_eq!((&started.0, &started.2), (step, candidate));
                        executions.push(started);
                    }
                    _ => {}
                }
            }
            let pick = |name: &str| ("pick".to_string(), 1, Some(name.to_string()));
            let plain = |step: &str, attempt| (step.to_string(), attempt, None);
            assert_eq!(
                executions,
                [
                    plain("build", 1),
                    plain("flaky", 1),
                    plain("flaky", 2),
                    plain("lint", 1),
                    pick("slow"),
                    pick("fast"),
                    plain("tolerant", 1),
                    // The second run executes only what did not hold.
                    plain("lint", 1),
                    plain("tolerant", 1),
                ]
            );
        }

        /// What `runs` left in `store`, one canonical JSON value per line.
        ///
        /// Stripped, because they differ from one run to the next: a manifest's
        /// `run_id`, `started_at_ms` and `duration_ms`, a report's `duration_ms`,
        /// and an observation's `started_at_ms`, `duration_ms` and `host_os`.
        /// Also a cache entry's `seal`, which follows from flot's version;
        /// `Store::recall` checks it instead.
        ///
        /// Named, so that a release leaves this as it is: flot's version, and the
        /// digests that follow from it or from the stripped fields. Each is
        /// checked before it is named: a receipt id is the digest of its
        /// identity, a step key the key of the inputs and dependencies its
        /// receipt records, a run root the root of its manifest's steps, and an
        /// observation digest is that of a stored receipt's observation. The
        /// run id a limitation quotes is named too.
        fn scenario_record(flow: &Flow, store: &Store, runs: &[RunReport]) -> String {
            use crate::core::digest;

            fn canonical<T: Serialize>(value: &T, strip: &[&str]) -> String {
                let text = digest::canonical_json(value)
                    .replace(
                        &format!("\"version\":\"{FLOT_VERSION}\""),
                        "\"version\":\"@VERSION@\"",
                    )
                    .replace(
                        &format!("\"flot_version\":\"{FLOT_VERSION}\""),
                        "\"flot_version\":\"@VERSION@\"",
                    );
                let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
                value
                    .as_object_mut()
                    .unwrap()
                    .retain(|key, _| !strip.contains(&key.as_str()));
                digest::canonical_bytes(&value)
            }

            // Receipts by the run and step that first name them.
            let mut labels: Vec<(String, String)> = Vec::new();
            let mut name = |receipt_id: &str, label: String, run: usize| {
                if labels.iter().any(|(id, _)| id == receipt_id) {
                    return;
                }
                let label = if labels.iter().any(|(_, taken)| *taken == label) {
                    format!("{label}.{run}")
                } else {
                    label
                };
                labels.push((receipt_id.to_string(), label));
            };
            for (index, run) in runs.iter().enumerate() {
                let number = index + 1;
                for step in &run.manifest.steps {
                    for (attempt, earlier) in step.attempts.iter().enumerate() {
                        name(
                            &earlier.receipt_id,
                            format!("{}#{}", step.step_id, attempt + 1),
                            number,
                        );
                    }
                    let kept = store.load_receipt(&step.receipt_id).unwrap();
                    let label = match &kept.identity.candidate {
                        Some(candidate) => format!("{}/{candidate}", step.step_id),
                        None => step.step_id.clone(),
                    };
                    name(&step.receipt_id, label, number);
                    for candidate in &step.candidates {
                        for (attempt, earlier) in candidate.attempts.iter().enumerate() {
                            name(
                                &earlier.receipt_id,
                                format!("{}/{}#{}", step.step_id, candidate.name, attempt + 1),
                                number,
                            );
                        }
                        name(
                            &candidate.receipt_id,
                            format!("{}/{}", step.step_id, candidate.name),
                            number,
                        );
                    }
                }
            }

            let mut stored: Vec<String> = std::fs::read_dir(store.root().join("receipts"))
                .unwrap()
                .map(|entry| {
                    let name = entry.unwrap().file_name().into_string().unwrap();
                    format!("sha256:{}", name.strip_suffix(".json").unwrap())
                })
                .collect();
            stored.sort();
            let mut named: Vec<String> = labels.iter().map(|(id, _)| id.clone()).collect();
            named.sort();
            assert_eq!(stored, named, "every stored receipt is one a run names");

            let mut names: Vec<(String, String)> = Vec::new();
            let mut lines: Vec<String> = Vec::new();
            for (receipt_id, label) in &labels {
                let receipt = store.load_receipt(receipt_id).unwrap();
                assert!(receipt.is_authentic(), "{label}");
                let identity = &receipt.identity;
                let step = flow.step(&identity.step_id).unwrap();
                let step = match &identity.candidate {
                    Some(candidate) => step.as_candidate(candidate),
                    None => step.clone(),
                };
                let facts = Facts {
                    inputs: identity.inputs.clone(),
                    verify_inputs: identity.verify_inputs.clone(),
                    limitations: Vec::new(),
                };
                assert_eq!(
                    identity.step_key,
                    keying::step_key(&flow.meta.name, &step, &facts, &identity.deps),
                    "{label}"
                );
                names.push((receipt_id.clone(), format!("@RECEIPT:{label}@")));
                names.push((receipt.observed_digest(), format!("@OBSERVED:{label}@")));
                if !names.iter().any(|(digest, _)| *digest == identity.step_key) {
                    names.push((identity.step_key.clone(), format!("@KEY:{label}@")));
                }
                lines.push(format!(
                    "receipt {label} identity {}",
                    canonical(identity, &[])
                ));
                lines.push(format!(
                    "receipt {label} observed {}",
                    canonical(
                        &receipt.observed,
                        &["started_at_ms", "duration_ms", "host_os"]
                    )
                ));
            }

            let mut entries: Vec<serde_json::Value> = std::fs::read_dir(store.root().join("cache"))
                .unwrap()
                .map(|entry| {
                    serde_json::from_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap())
                        .unwrap()
                })
                .collect();
            entries.sort_by_key(|entry| {
                let receipt_id = entry["receipt_id"].as_str().unwrap();
                labels.iter().position(|(id, _)| id == receipt_id)
            });
            for entry in &entries {
                let step_key = entry["step_key"].as_str().unwrap();
                let recalled = store.recall(step_key).unwrap().expect("a sealed entry");
                assert_eq!(
                    recalled.receipt.receipt_id,
                    entry["receipt_id"].as_str().unwrap()
                );
                lines.push(format!("cache {}", canonical(entry, &["seal"])));
            }

            let mut sections: Vec<String> = Vec::new();
            for (index, run) in runs.iter().enumerate() {
                let number = index + 1;
                let manifest = &run.manifest;
                let pairs: Vec<(String, String)> = manifest
                    .steps
                    .iter()
                    .map(|step| (step.step_id.clone(), step.receipt_id.clone()))
                    .collect();
                assert_eq!(manifest.root_digest, RunManifest::root_of(&pairs));
                assert_eq!(
                    manifest.run_id,
                    RunManifest::id_for(manifest.started_at_ms, &manifest.root_digest)
                );
                names.push((manifest.root_digest.clone(), format!("@ROOT:{number}@")));
                names.push((manifest.run_id.clone(), format!("@RUN:{number}@")));
                sections.push(format!(
                    "run {number} manifest {}",
                    canonical(manifest, &["run_id", "started_at_ms", "duration_ms"])
                ));
                for report in &run.steps {
                    sections.push(format!(
                        "run {number} report {} {}",
                        report.step_id,
                        canonical(report, &["duration_ms"])
                    ));
                }
            }
            sections.extend(lines);

            let mut record = sections.join("\n");
            record.push('\n');
            for (digest, name) in names {
                record = record.replace(&digest, &name);
            }
            record
        }

        const SCENARIO_RECORD: &str = r##"run 1 manifest {"claims":{"refuted":0,"unverified":2,"verified":3},"flot_version":"@VERSION@","flow":"scenario","flow_digest":"sha256:8ca2f9f300d6270baaede4b2f53d834abebfff569ee438772456c68392bce0d4","limitations":["build: ignored unknown metrics key `retries`; known keys are input_tokens, output_tokens, cost_usd","flaky: took 2 attempts of 2 allowed: 1 failed (exit 1), 2 held","lint: claims are unverified because the step did not succeed","pick: candidates slow held ($0.020000), fast held ($0.010000); kept fast","tolerant: input pattern `notes/*.md` matched no readable file; this step's cache key cannot see those changes","tolerant: exited 1 but the step is declared allow_failure, so the run is not failed by it","tolerant: produced no file for declared output report.txt, but the step is declared allow_failure","tolerant: changed files outside may_change (stray.txt), but the step is declared allow_failure","ship: not run: dependency lint (failed) did not hold","proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)","4 executed step(s) reported no metrics (flaky, flaky, lint, tolerant); the cost ceiling cannot see what they spent"],"max_cost_micro_usd":1000000,"max_parallel":1,"root_digest":"@ROOT:1@","spend":{"cost_micro_usd":280000,"input_tokens":7,"output_tokens":0,"reporting_steps":3,"reused_cost_micro_usd":0,"silent_steps":4,"unpriced_steps":0},"steps":[{"cached":false,"claims":{"refuted":0,"unverified":0,"verified":1},"observed_digest":"@OBSERVED:build@","receipt_id":"@RECEIPT:build@","status":"ok","step_id":"build"},{"attempts":[{"observed_digest":"@OBSERVED:flaky#1@","receipt_id":"@RECEIPT:flaky#1@"}],"cached":false,"claims":{"refuted":0,"unverified":0,"verified":1},"observed_digest":"@OBSERVED:flaky@","receipt_id":"@RECEIPT:flaky@","status":"ok","step_id":"flaky"},{"cached":false,"claims":{"refuted":0,"unverified":1,"verified":0},"observed_digest":"@OBSERVED:lint@","receipt_id":"@RECEIPT:lint@","status":"failed","step_id":"lint"},{"cached":false,"candidates":[{"name":"slow","observed_digest":"@OBSERVED:pick/slow@","receipt_id":"@RECEIPT:pick/slow@"}],"claims":{"refuted":0,"unverified":0,"verified":1},"observed_digest":"@OBSERVED:pick/fast@","receipt_id":"@RECEIPT:pick/fast@","status":"ok","step_id":"pick"},{"cached":false,"claims":{"refuted":0,"unverified":0,"verified":0},"observed_digest":"@OBSERVED:tolerant@","receipt_id":"@RECEIPT:tolerant@","status":"ok","step_id":"tolerant"},{"cached":false,"claims":{"refuted":0,"unverified":1,"verified":0},"observed_digest":"@OBSERVED:ship@","receipt_id":"@RECEIPT:ship@","status":"cancelled","step_id":"ship"}],"verdict":"failed"}
run 1 report build {"attempts":1,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":0,"verified":1},"limitations":[],"metrics":{"cost_micro_usd":250000,"input_tokens":7,"output_tokens":0},"outputs":[{"digest":"sha256:56f6e6304d02d413bb7d5d463ac5cdc58551266dc7269b467fc385815f39b913","path":"out.txt"}],"receipt_id":"@RECEIPT:build@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:efa19f8f1d5ded57870b2f1e3c2ce2e76bacfe57bd8b2461802e9b5c07b74e7e","stdout_inline":"building\n","stdout_truncated":false,"step_id":"build"}
run 1 report flaky {"attempts":2,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":0,"verified":1},"limitations":["took 2 attempts of 2 allowed: 1 failed (exit 1), 2 held"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:flaky@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"flaky"}
run 1 report lint {"attempts":1,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":1,"verified":0},"limitations":["claims are unverified because the step did not succeed"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:lint@","status":"failed","stderr_inline":"src.txt: 2 problems\n","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"lint"}
run 1 report pick {"attempts":1,"cached":false,"candidate":"fast","claims":{"refuted":0,"unverified":0,"verified":1},"limitations":["candidates slow held ($0.020000), fast held ($0.010000); kept fast"],"metrics":{"cost_micro_usd":10000,"input_tokens":0,"output_tokens":0},"outputs":[{"digest":"sha256:f10264dbe79d60da1b1973f97059e082f740991b0ee07c519a86eaa5fe94c6ea","path":"pick.txt"}],"receipt_id":"@RECEIPT:pick/fast@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"pick"}
run 1 report tolerant {"attempts":1,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":0,"verified":0},"limitations":["input pattern `notes/*.md` matched no readable file; this step's cache key cannot see those changes","exited 1 but the step is declared allow_failure, so the run is not failed by it","produced no file for declared output report.txt, but the step is declared allow_failure","changed files outside may_change (stray.txt), but the step is declared allow_failure"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:tolerant@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:2790f7552a86f9bb2620979635d19be349ace8f1bb0a147e0dfb24f686e97707","stdout_inline":"trying\n","stdout_truncated":false,"step_id":"tolerant"}
run 1 report ship {"attempts":0,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":1,"verified":0},"limitations":["not run: dependency lint (failed) did not hold"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:ship@","status":"cancelled","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"ship"}
run 2 manifest {"claims":{"refuted":0,"unverified":2,"verified":3},"flot_version":"@VERSION@","flow":"scenario","flow_digest":"sha256:8ca2f9f300d6270baaede4b2f53d834abebfff569ee438772456c68392bce0d4","limitations":["build: output out.txt was missing and was restored from its receipt","lint: claims are unverified because the step did not succeed","lint: its proof changed since run @RUN:1@ (lint_check.sh); read the change before trusting the verdict","tolerant: input pattern `notes/*.md` matched no readable file; this step's cache key cannot see those changes","tolerant: exited 1 but the step is declared allow_failure, so the run is not failed by it","tolerant: produced no file for declared output report.txt, but the step is declared allow_failure","ship: not run: dependency lint (failed) did not hold","proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)","2 executed step(s) reported no metrics (lint, tolerant); the cost ceiling cannot see what they spent"],"max_cost_micro_usd":1000000,"max_parallel":1,"root_digest":"@ROOT:2@","spend":{"cost_micro_usd":0,"input_tokens":0,"output_tokens":0,"reporting_steps":0,"reused_cost_micro_usd":260000,"silent_steps":2,"unpriced_steps":0},"steps":[{"cached":true,"claims":{"refuted":0,"unverified":0,"verified":1},"observed_digest":"@OBSERVED:build@","receipt_id":"@RECEIPT:build@","status":"ok","step_id":"build"},{"cached":true,"claims":{"refuted":0,"unverified":0,"verified":1},"observed_digest":"@OBSERVED:flaky@","receipt_id":"@RECEIPT:flaky@","status":"ok","step_id":"flaky"},{"cached":false,"claims":{"refuted":0,"unverified":1,"verified":0},"observed_digest":"@OBSERVED:lint.2@","receipt_id":"@RECEIPT:lint.2@","status":"failed","step_id":"lint"},{"cached":true,"claims":{"refuted":0,"unverified":0,"verified":1},"observed_digest":"@OBSERVED:pick/fast@","receipt_id":"@RECEIPT:pick/fast@","status":"ok","step_id":"pick"},{"cached":false,"claims":{"refuted":0,"unverified":0,"verified":0},"observed_digest":"@OBSERVED:tolerant.2@","receipt_id":"@RECEIPT:tolerant.2@","status":"ok","step_id":"tolerant"},{"cached":false,"claims":{"refuted":0,"unverified":1,"verified":0},"observed_digest":"@OBSERVED:ship.2@","receipt_id":"@RECEIPT:ship.2@","status":"cancelled","step_id":"ship"}],"verdict":"failed"}
run 2 report build {"attempts":0,"cached":true,"candidate":null,"claims":{"refuted":0,"unverified":0,"verified":1},"limitations":["output out.txt was missing and was restored from its receipt"],"metrics":{"cost_micro_usd":250000,"input_tokens":7,"output_tokens":0},"outputs":[{"digest":"sha256:56f6e6304d02d413bb7d5d463ac5cdc58551266dc7269b467fc385815f39b913","path":"out.txt"}],"receipt_id":"@RECEIPT:build@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:efa19f8f1d5ded57870b2f1e3c2ce2e76bacfe57bd8b2461802e9b5c07b74e7e","stdout_inline":"building\n","stdout_truncated":false,"step_id":"build"}
run 2 report flaky {"attempts":0,"cached":true,"candidate":null,"claims":{"refuted":0,"unverified":0,"verified":1},"limitations":[],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:flaky@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"flaky"}
run 2 report lint {"attempts":1,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":1,"verified":0},"limitations":["claims are unverified because the step did not succeed","its proof changed since run @RUN:1@ (lint_check.sh); read the change before trusting the verdict"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:lint.2@","status":"failed","stderr_inline":"src.txt: 2 problems\n","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"lint"}
run 2 report pick {"attempts":0,"cached":true,"candidate":"fast","claims":{"refuted":0,"unverified":0,"verified":1},"limitations":[],"metrics":{"cost_micro_usd":10000,"input_tokens":0,"output_tokens":0},"outputs":[{"digest":"sha256:f10264dbe79d60da1b1973f97059e082f740991b0ee07c519a86eaa5fe94c6ea","path":"pick.txt"}],"receipt_id":"@RECEIPT:pick/fast@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"pick"}
run 2 report tolerant {"attempts":1,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":0,"verified":0},"limitations":["input pattern `notes/*.md` matched no readable file; this step's cache key cannot see those changes","exited 1 but the step is declared allow_failure, so the run is not failed by it","produced no file for declared output report.txt, but the step is declared allow_failure"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:tolerant.2@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:2790f7552a86f9bb2620979635d19be349ace8f1bb0a147e0dfb24f686e97707","stdout_inline":"trying\n","stdout_truncated":false,"step_id":"tolerant"}
run 2 report ship {"attempts":0,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":1,"verified":0},"limitations":["not run: dependency lint (failed) did not hold"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:ship.2@","status":"cancelled","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"ship"}
receipt build identity {"claims":[{"reason":null,"status":"verified","text":"out.txt says built","verify_command":"sh check.sh","verify_exit":0,"verify_output_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}],"command":"printf 'built\\n' > out.txt; printf '{\"cost_usd\": 0.25, \"input_tokens\": 7, \"retries\": 1}' > \"$FLOT_METRICS\"; echo building","deps":[],"exit_code":0,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":[],"outputs":[{"digest":"sha256:56f6e6304d02d413bb7d5d463ac5cdc58551266dc7269b467fc385815f39b913","path":"out.txt"}],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:efa19f8f1d5ded57870b2f1e3c2ce2e76bacfe57bd8b2461802e9b5c07b74e7e","step_id":"build","step_key":"@KEY:build@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[{"digest":"sha256:c481f0032f2e89c470a45c9ae93832961e64b30676c875f3e8c95aa9d6d4e561","path":"check.sh"}]}
receipt build observed {"metrics":{"cost_micro_usd":250000,"input_tokens":7,"output_tokens":0},"metrics_notes":["ignored unknown metrics key `retries`; known keys are input_tokens, output_tokens, cost_usd"],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":9,"stdout_inline":"building\n","stdout_truncated":false}
receipt flaky#1 identity {"claims":[{"reason":"step did not succeed","status":"unverified","text":"it held","verify_command":"true","verify_exit":null,"verify_output_digest":null}],"command":"test \"$FLOT_ATTEMPT\" = 2","deps":[],"exit_code":1,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":["claims are unverified because the step did not succeed"],"outputs":[],"status":"failed","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"flaky","step_key":"@KEY:flaky#1@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt flaky#1 observed {"metrics":null,"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt flaky identity {"claims":[{"reason":null,"status":"verified","text":"it held","verify_command":"true","verify_exit":0,"verify_output_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}],"command":"test \"$FLOT_ATTEMPT\" = 2","deps":[],"exit_code":0,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":[],"outputs":[],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"flaky","step_key":"@KEY:flaky#1@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt flaky observed {"metrics":null,"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt lint identity {"claims":[{"reason":"step did not succeed","status":"unverified","text":"src.txt is clean","verify_command":"sh lint_check.sh","verify_exit":null,"verify_output_digest":null}],"command":"echo 'src.txt: 2 problems' >&2; exit 3","deps":[],"exit_code":3,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":["claims are unverified because the step did not succeed"],"outputs":[],"status":"failed","stderr_digest":"sha256:02745cd98faf87c5c3d6ca04b0427e3256769d9feb9b1d8e1ed330afd31c0916","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"lint","step_key":"@KEY:lint@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[{"digest":"sha256:28d3b9e880a77975493dc7e359144c0295a4f694cfe0af4f928c22307bc5c320","path":"lint_check.sh"}]}
receipt lint observed {"metrics":null,"metrics_notes":[],"stderr_bytes":20,"stderr_inline":"src.txt: 2 problems\n","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt pick/fast identity {"candidate":"fast","claims":[{"reason":null,"status":"verified","text":"pick.txt names a candidate","verify_command":"test -s pick.txt","verify_exit":0,"verify_output_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}],"command":"printf '%s\\n' \"$FLOT_CANDIDATE\" > pick.txt; if [ \"$FLOT_CANDIDATE\" = fast ]; then cost=0.01; else cost=0.02; fi; printf '{\"cost_usd\": %s}' \"$cost\" > \"$FLOT_METRICS\"","deps":[],"exit_code":0,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":[],"outputs":[{"digest":"sha256:f10264dbe79d60da1b1973f97059e082f740991b0ee07c519a86eaa5fe94c6ea","path":"pick.txt"}],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"pick","step_key":"@KEY:pick/fast@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt pick/fast observed {"metrics":{"cost_micro_usd":10000,"input_tokens":0,"output_tokens":0},"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt pick/slow identity {"candidate":"slow","claims":[{"reason":null,"status":"verified","text":"pick.txt names a candidate","verify_command":"test -s pick.txt","verify_exit":0,"verify_output_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}],"command":"printf '%s\\n' \"$FLOT_CANDIDATE\" > pick.txt; if [ \"$FLOT_CANDIDATE\" = fast ]; then cost=0.01; else cost=0.02; fi; printf '{\"cost_usd\": %s}' \"$cost\" > \"$FLOT_METRICS\"","deps":[],"exit_code":0,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":[],"outputs":[{"digest":"sha256:4c4a4a89dddfad0c9d436f7b5ebf11fc390938a70a40e4226b4bc8cd423ba83e","path":"pick.txt"}],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"pick","step_key":"@KEY:pick/slow@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt pick/slow observed {"metrics":{"cost_micro_usd":20000,"input_tokens":0,"output_tokens":0},"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt tolerant identity {"changed":{"stray.txt":"sha256:43bab6c26bc03299f3e5108f37cfa190ef6446cfe38f4229204a0d6b88e4b102","trace.log":"sha256:ea4f8e5663bf24ef621b5f3bfe0f251e1f9d63d25cfdab35a3ff33ac277d8cc6"},"claims":[],"command":"echo trying; printf 'trace\\n' > trace.log; printf 'stray\\n' > stray.txt; exit 1","deps":[],"exit_code":1,"flow":"scenario","inputs":[],"limitations":["input pattern `notes/*.md` matched no readable file; this step's cache key cannot see those changes","exited 1 but the step is declared allow_failure, so the run is not failed by it","produced no file for declared output report.txt, but the step is declared allow_failure","changed files outside may_change (stray.txt), but the step is declared allow_failure"],"outputs":[],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:2790f7552a86f9bb2620979635d19be349ace8f1bb0a147e0dfb24f686e97707","step_id":"tolerant","step_key":"@KEY:tolerant@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt tolerant observed {"metrics":null,"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":7,"stdout_inline":"trying\n","stdout_truncated":false}
receipt ship identity {"claims":[{"reason":"step was not run","status":"unverified","text":"shipped","verify_command":"true","verify_exit":null,"verify_output_digest":null}],"command":"cat out.txt","deps":[{"receipt_id":"@RECEIPT:build@","step_id":"build"},{"receipt_id":"@RECEIPT:lint@","step_id":"lint"}],"exit_code":null,"flow":"scenario","inputs":[{"digest":"sha256:56f6e6304d02d413bb7d5d463ac5cdc58551266dc7269b467fc385815f39b913","path":"out.txt"}],"limitations":["not run: dependency lint (failed) did not hold"],"outputs":[],"status":"cancelled","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"ship","step_key":"@KEY:ship@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt ship observed {"metrics":null,"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt lint.2 identity {"claims":[{"reason":"step did not succeed","status":"unverified","text":"src.txt is clean","verify_command":"sh lint_check.sh","verify_exit":null,"verify_output_digest":null}],"command":"echo 'src.txt: 2 problems' >&2; exit 3","deps":[],"exit_code":3,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":["claims are unverified because the step did not succeed"],"outputs":[],"status":"failed","stderr_digest":"sha256:02745cd98faf87c5c3d6ca04b0427e3256769d9feb9b1d8e1ed330afd31c0916","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"lint","step_key":"@KEY:lint.2@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[{"digest":"sha256:0c6868c2c44f053619cef1cc383e1d530743b574ca192ace9168a9ccf46a86e3","path":"lint_check.sh"}]}
receipt lint.2 observed {"metrics":null,"metrics_notes":[],"stderr_bytes":20,"stderr_inline":"src.txt: 2 problems\n","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt tolerant.2 identity {"changed":{"stray.txt":"sha256:43bab6c26bc03299f3e5108f37cfa190ef6446cfe38f4229204a0d6b88e4b102","trace.log":"sha256:ea4f8e5663bf24ef621b5f3bfe0f251e1f9d63d25cfdab35a3ff33ac277d8cc6"},"claims":[],"command":"echo trying; printf 'trace\\n' > trace.log; printf 'stray\\n' > stray.txt; exit 1","deps":[],"exit_code":1,"flow":"scenario","inputs":[],"limitations":["input pattern `notes/*.md` matched no readable file; this step's cache key cannot see those changes","exited 1 but the step is declared allow_failure, so the run is not failed by it","produced no file for declared output report.txt, but the step is declared allow_failure"],"outputs":[],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:2790f7552a86f9bb2620979635d19be349ace8f1bb0a147e0dfb24f686e97707","step_id":"tolerant","step_key":"@KEY:tolerant@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt tolerant.2 observed {"metrics":null,"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":7,"stdout_inline":"trying\n","stdout_truncated":false}
receipt ship.2 identity {"claims":[{"reason":"step was not run","status":"unverified","text":"shipped","verify_command":"true","verify_exit":null,"verify_output_digest":null}],"command":"cat out.txt","deps":[{"receipt_id":"@RECEIPT:build@","step_id":"build"},{"receipt_id":"@RECEIPT:lint.2@","step_id":"lint"}],"exit_code":null,"flow":"scenario","inputs":[{"digest":"sha256:56f6e6304d02d413bb7d5d463ac5cdc58551266dc7269b467fc385815f39b913","path":"out.txt"}],"limitations":["not run: dependency lint (failed) did not hold"],"outputs":[],"status":"cancelled","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"ship","step_key":"@KEY:ship.2@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt ship.2 observed {"metrics":null,"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
cache {"receipt_id":"@RECEIPT:build@","step_key":"@KEY:build@","vouched_by":[]}
cache {"receipt_id":"@RECEIPT:flaky@","step_key":"@KEY:flaky#1@","vouched_by":[]}
cache {"receipt_id":"@RECEIPT:pick/fast@","step_key":"@KEY:pick/fast@","vouched_by":[]}
cache {"receipt_id":"@RECEIPT:pick/slow@","step_key":"@KEY:pick/slow@","vouched_by":[]}
"##;
    }
}
