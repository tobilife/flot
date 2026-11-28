//! Whether a run's manifest follows from the receipts it names: the one rule
//! import and audit both judge a run by.

mod attempts;
mod belongs;

use std::collections::BTreeMap;

use super::{
    candidate_rank, sum_costs, verdict_for, ClaimCounts, Observed, Receipt, RunManifest, RunStep,
    StepStatus,
};
use crate::core::digest;
use crate::core::metrics::Spend;

/// What a run's receipts and observations are looked up in, by receipt id
/// and by observation digest.
struct Records<'a> {
    receipts: &'a BTreeMap<&'a str, &'a Receipt>,
    observations: &'a BTreeMap<&'a str, Observed>,
}

impl RunManifest {
    /// Everything in this manifest that does not follow from the receipts it
    /// names. Its run id, step list, per-step status and claim counts, totals,
    /// verdict and root are all derived facts, so each is recomputed rather
    /// than believed; a manifest is only as good as its receipts. Each receipt
    /// must also be authentic, belong to this flow and step, point at the
    /// dependency receipts this run recorded, and have its limitations listed
    /// on the manifest. Import and audit both judge a run by this one rule. `receipts`
    /// maps receipt id to receipt and `observations` maps observation digest to
    /// observation; a step whose receipt or observation is absent is reported.
    pub fn disagreements(
        &self,
        receipts: &BTreeMap<&str, &Receipt>,
        observations: &BTreeMap<&str, Observed>,
    ) -> Vec<String> {
        let records = Records {
            receipts,
            observations,
        };
        let mut found = Vec::new();
        if self.run_id != Self::id_for(self.started_at_ms, &self.root_digest) {
            found.push(format!(
                "run id `{}` is not derived from its start time and root",
                self.run_id
            ));
        }
        // Whether a run selected every step cannot be read off its receipts;
        // a signature covers that. What can be checked is that a selection
        // lists the steps it names and leaves some out.
        if let Some(partial) = self.partial {
            if partial.selected >= partial.of {
                found.push(format!(
                    "partial run of {} of {} steps leaves no step out",
                    partial.selected, partial.of
                ));
            }
            if partial.selected != self.steps.len() {
                found.push(format!(
                    "partial run of {} of {} steps lists {} step(s)",
                    partial.selected,
                    partial.of,
                    self.steps.len()
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut statuses = Vec::new();
        let mut totals = ClaimCounts::default();
        let mut spend = Spend::default();
        for step in &self.steps {
            if !crate::core::flow::is_valid_step_id(&step.step_id) {
                found.push(format!("step id `{}` is not a valid step id", step.step_id));
            }
            if !seen.insert(step.step_id.as_str()) {
                found.push(format!("step `{}` appears more than once", step.step_id));
            }
            // Only a reused receipt rests on someone else's word, and a
            // signer is named once, spelled as flot spells keys, so a report
            // can match it against attestations and the trust list.
            if !step.vouched_by.is_empty() && !step.cached {
                found.push(format!(
                    "{}: names signers who vouched for it, but the step was not reused",
                    step.step_id
                ));
            }
            let mut signers = std::collections::BTreeSet::new();
            for key in &step.vouched_by {
                if crate::core::attest::canonical_key(key).ok().as_deref() != Some(key.as_str()) {
                    found.push(format!(
                        "{}: `{}` is not a signer key as flot writes one",
                        step.step_id,
                        crate::core::attest::fingerprint(key)
                    ));
                }
                if !signers.insert(key.as_str()) {
                    found.push(format!(
                        "{}: signer {} is named more than once",
                        step.step_id,
                        crate::core::attest::fingerprint(key)
                    ));
                }
            }
            let Some(receipt) = receipts.get(step.receipt_id.as_str()) else {
                found.push(format!(
                    "{}: receipt {} is not available",
                    step.step_id,
                    digest::short(&step.receipt_id)
                ));
                continue;
            };
            self.belongs(&step.step_id, receipt, &mut found);
            if step.cached && !(step.attempts.is_empty() && step.candidates.is_empty()) {
                found.push(format!(
                    "{}: a reused receipt lists attempts this run made",
                    step.step_id
                ));
            }
            let kept_cost = self.attempts_judged(
                &step.step_id,
                &step.step_id,
                &step.attempts,
                &records,
                &mut spend,
                &mut found,
            );
            let kept_cost = sum_costs(
                kept_cost,
                observations
                    .get(step.observed_digest.as_str())
                    .and_then(|observed| observed.metrics)
                    .and_then(|metrics| metrics.cost_micro_usd),
            );
            if !step.candidates.is_empty() && receipt.identity.candidate.is_none() {
                found.push(format!(
                    "{}: the run lists other candidates but the receipt names none",
                    step.step_id
                ));
            }
            let mut names = std::collections::BTreeSet::new();
            names.extend(receipt.identity.candidate.as_deref());
            for candidate in &step.candidates {
                let label = format!("{} candidate {}", step.step_id, candidate.name);
                if !names.insert(candidate.name.as_str()) {
                    found.push(format!("{label}: the candidate appears more than once"));
                }
                let earlier_cost = self.attempts_judged(
                    &step.step_id,
                    &label,
                    &candidate.attempts,
                    &records,
                    &mut spend,
                    &mut found,
                );
                let Some(last) = receipts.get(candidate.receipt_id.as_str()) else {
                    found.push(format!(
                        "{label}: receipt {} is not available",
                        digest::short(&candidate.receipt_id)
                    ));
                    continue;
                };
                self.belongs(&label, last, &mut found);
                if last.identity.step_id != step.step_id {
                    found.push(format!(
                        "{label}: receipt belongs to step `{}`",
                        last.identity.step_id
                    ));
                }
                if last.identity.candidate.as_deref() != Some(candidate.name.as_str()) {
                    found.push(format!("{label}: receipt is not that candidate's"));
                }
                let Some(observed) = observations.get(candidate.observed_digest.as_str()) else {
                    found.push(format!(
                        "{label}: observation {} is not available",
                        digest::short(&candidate.observed_digest)
                    ));
                    continue;
                };
                spend.add_executed(observed.metrics);
                let cost = sum_costs(
                    earlier_cost,
                    observed.metrics.and_then(|metrics| metrics.cost_micro_usd),
                );
                // The kept candidate must rank at least as well as each
                // other one, by the rule that chose it.
                if candidate_rank(last, cost) < candidate_rank(receipt, kept_cost) {
                    found.push(format!(
                        "{label}: it ranks above the candidate the run kept"
                    ));
                }
            }
            if receipt.identity.step_id != step.step_id {
                found.push(format!(
                    "{}: receipt belongs to step `{}`",
                    step.step_id, receipt.identity.step_id
                ));
            }
            // What the receipt says went wrong must be on the manifest: a
            // manifest that drops a step's limitation reads cleaner than the
            // work was.
            for limitation in &receipt.identity.limitations {
                let listed = format!("{}: {}", step.step_id, limitation);
                if !self.limitations.contains(&listed) {
                    found.push(format!(
                        "{}: manifest omits the receipt's limitation `{}`",
                        step.step_id, limitation
                    ));
                }
            }
            if receipt.identity.status != step.status {
                found.push(format!(
                    "{}: manifest says {} but the receipt says {}",
                    step.step_id,
                    step.status.label(),
                    receipt.identity.status.label()
                ));
            }
            let Some(observed) = observations.get(step.observed_digest.as_str()) else {
                found.push(format!(
                    "{}: observation {} is not available",
                    step.step_id,
                    digest::short(&step.observed_digest)
                ));
                continue;
            };
            if step.cached {
                spend.add_reused(observed.metrics);
            } else if receipt.identity.status != StepStatus::Cancelled {
                spend.add_executed(observed.metrics);
            }
            let counts = receipt.claim_counts();
            if counts != step.claims {
                found.push(format!(
                    "{}: manifest claim counts disagree with the receipt",
                    step.step_id
                ));
            }
            totals.add(counts);
            statuses.push(RunStep {
                status: receipt.identity.status,
                claims: counts,
                ..step.clone()
            });
        }
        if found.is_empty() {
            if totals != self.claims {
                found.push("run claim totals disagree with its receipts".into());
            }
            if spend != self.spend {
                found.push("run spend disagrees with what its receipts reported".into());
            }
            let verdict = verdict_for(&statuses, totals);
            if verdict != self.verdict {
                found.push(format!(
                    "manifest verdict is {} but its receipts make it {}",
                    self.verdict.label(),
                    verdict.label()
                ));
            }
        }
        let pairs: Vec<(String, String)> = self
            .steps
            .iter()
            .map(|step| (step.step_id.clone(), step.receipt_id.clone()))
            .collect();
        if Self::root_of(&pairs) != self.root_digest {
            found.push("run root digest does not match its steps".into());
        }
        found
    }
}

#[cfg(test)]
mod tests;
