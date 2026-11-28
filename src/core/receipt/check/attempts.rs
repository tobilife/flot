//! Judging the earlier attempts a run made at a step, and adding what they
//! spent.

use crate::core::digest;
use crate::core::metrics::Spend;
use crate::core::receipt::{sum_costs, RunAttempt, RunManifest};

use super::Records;

impl RunManifest {
    /// Judge a list of earlier attempts at a step and add what they spent.
    /// Returns their total reported cost, `None` when any of them is unknown.
    pub(super) fn attempts_judged(
        &self,
        step_id: &str,
        label: &str,
        attempts: &[RunAttempt],
        records: &Records,
        spend: &mut Spend,
        found: &mut Vec<String>,
    ) -> Option<u64> {
        let mut cost = Some(0);
        for (number, attempt) in attempts.iter().enumerate() {
            let label = format!("{label} attempt {}", number + 1);
            let Some(earlier) = records.receipts.get(attempt.receipt_id.as_str()) else {
                found.push(format!(
                    "{label}: receipt {} is not available",
                    digest::short(&attempt.receipt_id)
                ));
                continue;
            };
            self.belongs(&label, earlier, found);
            if earlier.identity.step_id != step_id {
                found.push(format!(
                    "{label}: receipt belongs to step `{}`",
                    earlier.identity.step_id
                ));
            }
            // The step is only tried again when an attempt did not hold.
            if earlier.is_reusable() {
                found.push(format!(
                    "{label}: it held, so the step would not have been tried again"
                ));
            }
            match records.observations.get(attempt.observed_digest.as_str()) {
                Some(observed) => {
                    spend.add_executed(observed.metrics);
                    cost = sum_costs(cost, observed.metrics.and_then(|m| m.cost_micro_usd));
                }
                None => {
                    cost = None;
                    found.push(format!(
                        "{label}: observation {} is not available",
                        digest::short(&attempt.observed_digest)
                    ))
                }
            }
        }
        cost
    }
}
