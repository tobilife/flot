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
mod plan;
mod reuse;
mod run;

pub use self::plan::PlanEntry;

/// Why the claims of a step whose sandbox cannot be enforced here stay
/// unverified: the step did not run, as one that asked to be confined runs
/// confined or not at all.
pub const UNCONFINED: &str = "step could not run in its sandbox";

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, OnceLock, RwLock};

use anyhow::Result;
use serde::Serialize;

use crate::core::attest::VouchedStep;
use crate::core::flow::{Flow, Step};
use crate::core::metrics::Metrics;
use crate::core::receipt::{
    sum_costs, ClaimCounts, DepRef, FileDigest, Receipt, RunManifest, StepStatus,
};
use crate::engine::baseline::Baseline;
use crate::engine::claims::ProofOutcome;
use crate::engine::exec;
use crate::engine::keying::{self, Facts};
use crate::engine::progress;
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

/// Unique metrics paths for concurrently executing steps.
static METRICS_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Spend known at the moment a step is about to execute.
#[derive(Debug, Clone, Copy)]
struct CostGate {
    ceiling_micro_usd: u64,
    spent_micro_usd: u64,
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
            commit: baseline.commit.clone(),
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
}

#[cfg(test)]
mod tests;
