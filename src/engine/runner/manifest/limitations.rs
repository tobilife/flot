//! What a reader of the run's verdict must know: each step's limitations,
//! and what the run's proofs were held to.

use std::collections::HashMap;

use crate::core::flow::Step;
use crate::core::metrics;
use crate::core::receipt::{Receipt, ReceiptIdentity};
use crate::engine::keying;
use crate::engine::runner::{Contender, Runner, Settled};

impl Runner<'_> {
    /// What a reader of a step's verdict in this run must know: its
    /// receipt's limitations and the run's notes on it, then whether its
    /// proof changed since it last ran here, which candidates ran, and how
    /// many attempts it took.
    pub(super) fn step_limitations(
        &self,
        step_id: &str,
        entry: &Settled,
        proofs_before: &HashMap<String, (String, Option<ReceiptIdentity>)>,
    ) -> Vec<String> {
        let receipt = &entry.receipt;
        let step = self.flow.step(step_id).expect("validated step");
        let mut limitations = receipt.identity.limitations.clone();
        limitations.extend(entry.notes.iter().cloned());
        if let Some((run_id, Some(earlier))) = proofs_before.get(step_id) {
            let changed = proof_changes(earlier, &receipt.identity);
            if !changed.is_empty() {
                limitations.push(format!(
                    "its proof changed since run {run_id} ({}); read the change before trusting the verdict",
                    changed.join(", ")
                ));
            }
        }
        if !entry.cached && receipt.identity.candidate.is_some() {
            limitations.push(candidates_told(step, entry));
        }
        if !entry.attempts.is_empty() {
            limitations.push(attempts_told(step, entry));
        }
        limitations
    }

    /// What the run's proofs were held to: the revision of its baseline, and
    /// the steps with claims there that this flow lacks; or, without one,
    /// nothing that stops an edited proof from passing.
    pub(super) fn proof_limitations(&self, order: &[String]) -> Vec<String> {
        let mut limitations = Vec::new();
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
        limitations
    }

    /// For each step of `order` with a proof, the newest run executed here
    /// that has it, and its receipt there (`None` when that receipt is
    /// unreadable). Imported runs are not this host's, and a run of another
    /// flow is not this step's.
    pub(super) fn proofs_before(
        &self,
        order: &[String],
    ) -> HashMap<String, (String, Option<ReceiptIdentity>)> {
        let mut found = HashMap::new();
        // Held to no revision, a proof can be edited along with the work it
        // judges; what it read last time here is the one thing to compare.
        if self.baseline.is_some() {
            return found;
        }
        // Only a step with a proof has one to compare, and the store is read
        // until each such step is found.
        let steps: Vec<String> = order
            .iter()
            .filter(|step_id| {
                self.flow
                    .step(step_id)
                    .is_some_and(|step| step.verify.is_some())
            })
            .cloned()
            .collect();
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

/// Which candidates of a step ran, in flow order, how each ended and what
/// it cost, and which was kept.
fn candidates_told(step: &Step, entry: &Settled) -> String {
    let receipt = &entry.receipt;
    let kept = Contender {
        name: receipt.identity.candidate.clone().unwrap_or_default(),
        receipt: receipt.clone(),
        attempts: entry.attempts.clone(),
    };
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
    format!("candidates {}; kept {}", told.join(", "), kept.name)
}

/// How each attempt at a step ended, of the attempts it is allowed.
fn attempts_told(step: &Step, entry: &Settled) -> String {
    let mut told: Vec<String> = entry
        .attempts
        .iter()
        .enumerate()
        .map(|(number, earlier)| format!("{} {}", number + 1, outcome(earlier)))
        .collect();
    told.push(format!("{} {}", told.len() + 1, outcome(&entry.receipt)));
    format!(
        "took {} attempts of {} allowed: {}",
        told.len(),
        step.attempts(),
        told.join(", ")
    )
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
