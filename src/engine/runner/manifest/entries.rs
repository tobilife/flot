//! A settled step added to the manifest being put together: its entry and
//! its report, and what its executions spent against the cost ceiling.

use anyhow::Result;

use crate::core::metrics;
use crate::core::receipt::{Receipt, RunAttempt, RunCandidate, RunStep, StepStatus};
use crate::engine::runner::{Settled, StepReport};
use crate::store::Store;

use super::Assembly;

impl Assembly {
    /// Add a settled step: its manifest entry and its report, what its
    /// executions spent, and its `limitations`, each named by the step.
    pub(super) fn add(
        &mut self,
        store: &Store,
        step_id: &str,
        entry: &Settled,
        limitations: Vec<String>,
    ) -> Result<()> {
        let receipt = &entry.receipt;
        let cached = entry.cached;
        let counts = receipt.claim_counts();
        self.totals.add(counts);
        let mut attempts = Vec::new();
        for earlier in &entry.attempts {
            attempts.push(self.executed(store, earlier)?);
        }
        let mut candidates = Vec::new();
        for candidate in &entry.candidates {
            let mut attempts = Vec::new();
            for earlier in candidate.attempts.iter().chain([&candidate.receipt]) {
                attempts.push(self.executed(store, earlier)?);
            }
            let last = attempts.pop().expect("a candidate has a last receipt");
            candidates.push(RunCandidate {
                name: candidate.name.clone(),
                receipt_id: last.receipt_id,
                observed_digest: last.observed_digest,
                attempts,
            });
        }
        if cached {
            self.spend.add_reused(receipt.observed.metrics);
        } else if receipt.identity.status != StepStatus::Cancelled {
            self.spend.add_executed(receipt.observed.metrics);
            self.unknown_cost(receipt);
        }
        for limitation in &limitations {
            self.limitations
                .push(format!("{}: {}", step_id, limitation));
        }
        if !cached {
            for note in &receipt.observed.metrics_notes {
                self.limitations.push(format!("{}: {}", step_id, note));
            }
        }
        self.steps.push(RunStep {
            step_id: step_id.to_string(),
            receipt_id: receipt.receipt_id.clone(),
            status: receipt.identity.status,
            cached,
            claims: counts,
            observed_digest: store.put_observation(&receipt.observed)?,
            attempts,
            candidates,
            vouched_by: entry.vouched_by.clone(),
        });
        self.reports.push(StepReport {
            step_id: step_id.to_string(),
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
            limitations,
            metrics: receipt.observed.metrics,
            outputs: receipt.identity.outputs.clone(),
            candidate: receipt.identity.candidate.clone(),
            attempts: if cached || receipt.identity.status == StepStatus::Cancelled {
                0
            } else {
                entry.attempts.len() + 1
            },
        });
        Ok(())
    }

    /// Count what one execution of a step spent, and record it as an attempt.
    fn executed(&mut self, store: &Store, receipt: &Receipt) -> Result<RunAttempt> {
        self.spend.add_executed(receipt.observed.metrics);
        self.unknown_cost(receipt);
        Ok(RunAttempt {
            receipt_id: receipt.receipt_id.clone(),
            observed_digest: store.put_observation(&receipt.observed)?,
        })
    }

    /// Note an execution whose cost is unknown, named by its step and, for a
    /// candidate, the candidate.
    fn unknown_cost(&mut self, receipt: &Receipt) {
        let label = match &receipt.identity.candidate {
            Some(name) => format!("{} ({name})", receipt.identity.step_id),
            None => receipt.identity.step_id.clone(),
        };
        match receipt.observed.metrics {
            None => self.silent.push(label),
            Some(metrics) if metrics.cost_micro_usd.is_none() => self.unpriced.push(label),
            Some(_) => {}
        }
    }

    /// Under a cost ceiling: the executions it could not see, and how far
    /// the run went past it.
    pub(super) fn cost_limitations(&mut self) {
        let Some(ceiling) = self.ceiling_micro_usd else {
            return;
        };
        if !self.silent.is_empty() {
            self.limitations.push(format!(
                "{} executed step(s) reported no metrics ({}); the cost ceiling cannot see what they spent",
                self.silent.len(),
                self.silent.join(", ")
            ));
        }
        if !self.unpriced.is_empty() {
            self.limitations.push(format!(
                "{} executed step(s) reported no cost ({}); the cost ceiling cannot see what they spent",
                self.unpriced.len(),
                self.unpriced.join(", ")
            ));
        }
        if self.spend.cost_micro_usd > ceiling {
            self.limitations.push(format!(
                "spent {} against a {} ceiling: the ceiling is checked before each step starts, so steps already running when it was crossed finished",
                metrics::format_usd(self.spend.cost_micro_usd),
                metrics::format_usd(ceiling)
            ));
        }
    }
}
