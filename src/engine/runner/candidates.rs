//! A step with candidates: each runs in its own copy of the workspace, and
//! the best reusable receipt among them stands in for running them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

use anyhow::Result;

use crate::core::flow::Step;
use crate::core::metrics;
use crate::core::receipt::{candidate_rank, DepRef, FileDigest, StepStatus};
use crate::engine::exec;
use crate::engine::keying;
use crate::store::Recalled;

use super::{Contender, CostGate, Runner};

/// Unique scratch workspaces for candidates.
static WORKSPACE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl Runner<'_> {
    /// The best reusable receipt among a step's candidates, ranked as when
    /// they ran, with its missing outputs put back.
    pub(super) fn reuse_candidate(
        &self,
        step: &Step,
        deps: &[DepRef],
        notes: &mut Vec<String>,
    ) -> Option<(Recalled, Vec<String>)> {
        let recalled = self.best_cached_candidate(step, deps, &[], notes)?;
        match self.restore_outputs(&recalled.receipt) {
            Ok(restored) => Some((recalled, restored)),
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
    pub(super) fn best_cached_candidate(
        &self,
        step: &Step,
        deps: &[DepRef],
        pending: &[FileDigest],
        notes: &mut Vec<String>,
    ) -> Option<Recalled> {
        let mut misses = Vec::new();
        let mut known = Vec::new();
        for name in &step.candidates {
            let candidate = step.as_candidate(name);
            let facts = keying::facts_with(&self.root, &candidate, pending);
            let key = self.step_key(&candidate, &facts, deps);
            match self.recall(&candidate, &key, &mut misses) {
                Ok(Some(recalled)) => known.push(recalled),
                Ok(None) => {}
                Err(error) => misses.push(format!(
                    "cached receipt of candidate {name} not reused: {error:#}"
                )),
            }
        }
        let best = (0..known.len()).min_by_key(|&index| {
            let receipt = &known[index].receipt;
            let cost = receipt
                .observed
                .metrics
                .and_then(|metrics| metrics.cost_micro_usd);
            (candidate_rank(receipt, cost), index)
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
    pub(super) fn compete(
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
                latest: OnceLock::new(),
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
}
