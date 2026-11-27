//! Whether a run's manifest follows from the receipts it names: the one rule
//! import and audit both judge a run by.

use std::collections::BTreeMap;

use super::{
    candidate_rank, sum_costs, verdict_for, ClaimCounts, ClaimStatus, Observed, Receipt,
    RunAttempt, RunManifest, RunStep, StepStatus,
};
use crate::core::digest;
use crate::core::metrics::Spend;

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
                receipts,
                observations,
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
                    receipts,
                    observations,
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

    /// Judge a list of earlier attempts at a step and add what they spent.
    /// Returns their total reported cost, `None` when any of them is unknown.
    #[allow(clippy::too_many_arguments)]
    fn attempts_judged(
        &self,
        step_id: &str,
        label: &str,
        attempts: &[RunAttempt],
        receipts: &BTreeMap<&str, &Receipt>,
        observations: &BTreeMap<&str, Observed>,
        spend: &mut Spend,
        found: &mut Vec<String>,
    ) -> Option<u64> {
        let mut cost = Some(0);
        for (number, attempt) in attempts.iter().enumerate() {
            let label = format!("{label} attempt {}", number + 1);
            let Some(earlier) = receipts.get(attempt.receipt_id.as_str()) else {
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
            match observations.get(attempt.observed_digest.as_str()) {
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

    /// What every receipt a run names must be: authentic, of this flow, with
    /// proof behind each verified claim, built on the receipts this run
    /// recorded for its dependencies.
    fn belongs(&self, label: &str, receipt: &Receipt, found: &mut Vec<String>) {
        if !receipt.is_authentic() {
            found.push(format!(
                "{label}: receipt body does not match its id {}",
                digest::short(&receipt.receipt_id)
            ));
        }
        for dependency in &receipt.identity.deps {
            match self.step(&dependency.step_id) {
                Some(recorded) if recorded.receipt_id == dependency.receipt_id => {}
                Some(_) => found.push(format!(
                    "{label}: references a different receipt for `{}` than the run recorded",
                    dependency.step_id
                )),
                None => found.push(format!(
                    "{label}: references `{}`, which is not in this run",
                    dependency.step_id
                )),
            }
        }
        if receipt.identity.claims.iter().any(|claim| {
            claim.status == ClaimStatus::Verified && claim.verify_output_digest.is_none()
        }) {
            found.push(format!(
                "{label}: claim is marked verified with no proof object"
            ));
        }
        if receipt.identity.flow != self.flow {
            found.push(format!(
                "{label}: receipt belongs to flow `{}`",
                receipt.identity.flow
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::identity;
    use super::super::*;

    /// A one-step run whose manifest follows exactly from its receipt.
    fn coherent(receipt: &Receipt) -> RunManifest {
        let observed_digest = digest::bytes(b"observation");
        let steps = vec![RunStep {
            observed_digest,
            step_id: receipt.identity.step_id.clone(),
            receipt_id: receipt.receipt_id.clone(),
            status: receipt.identity.status,
            cached: false,
            claims: receipt.claim_counts(),
            attempts: Vec::new(),
            candidates: Vec::new(),
            vouched_by: Vec::new(),
        }];
        let root_digest =
            RunManifest::root_of(&[(steps[0].step_id.clone(), steps[0].receipt_id.clone())]);
        RunManifest {
            run_id: RunManifest::id_for(1, &root_digest),
            flow: receipt.identity.flow.clone(),
            flow_digest: String::new(),
            root_digest,
            verdict: verdict_for(&steps, receipt.claim_counts()),
            partial: None,
            claims: receipt.claim_counts(),
            limitations: receipt
                .identity
                .limitations
                .iter()
                .map(|limitation| format!("{}: {limitation}", receipt.identity.step_id))
                .collect(),
            steps,
            max_parallel: 1,
            spend: {
                let mut spend = Spend::default();
                spend.add_executed(Observed::default().metrics);
                spend
            },
            max_cost_micro_usd: None,
            started_at_ms: 1,
            duration_ms: 0,
            flot_version: String::new(),
            refusals: Vec::new(),
        }
    }

    fn judge(manifest: &RunManifest, receipt: &Receipt) -> Vec<String> {
        let receipts = BTreeMap::from([(receipt.receipt_id.as_str(), receipt)]);
        let observations = BTreeMap::from([(
            manifest.steps[0].observed_digest.as_str(),
            Observed::default(),
        )]);
        manifest.disagreements(&receipts, &observations)
    }

    #[test]
    fn a_selection_lists_the_steps_it_names_and_a_signature_covers_it() {
        let receipt = Receipt::seal(identity("a", "out"), Observed::default());
        let mut partial = coherent(&receipt);
        partial.partial = Some(Partial { selected: 1, of: 2 });
        assert_eq!(judge(&partial, &receipt), Vec::<String>::new());
        // A run of every step records no selection, so its bytes are as
        // they were before selections were recorded.
        assert!(!serde_json::to_string(&coherent(&receipt))
            .unwrap()
            .contains("partial"));

        for (selected, of, expected) in [
            (1, 1, "partial run of 1 of 1 steps leaves no step out"),
            (2, 3, "partial run of 2 of 3 steps lists 1 step(s)"),
        ] {
            let mut forged = coherent(&receipt);
            forged.partial = Some(Partial { selected, of });
            assert_eq!(judge(&forged, &receipt), vec![expected.to_string()]);
        }

        // Dropping the selection from a signed run breaks the signature.
        let attestation = crate::core::attest::sign(&[7; 32], &partial);
        assert_eq!(crate::core::attest::check(&attestation, &partial), Ok(()));
        let mut whole = partial.clone();
        whole.partial = None;
        assert!(crate::core::attest::check(&attestation, &whole).is_err());
    }

    #[test]
    fn only_a_reused_step_names_its_signers_each_once_by_key() {
        let receipt = Receipt::seal(identity("a", "out"), Observed::default());
        let key = crate::core::attest::public_key_of(&[7; 32]);
        let mut vouched = coherent(&receipt);
        vouched.steps[0].cached = true;
        vouched.steps[0].vouched_by = vec![key.clone()];
        vouched.spend = Spend::default();
        vouched.spend.add_reused(Observed::default().metrics);
        assert_eq!(judge(&vouched, &receipt), Vec::<String>::new());

        let mut executed = coherent(&receipt);
        executed.steps[0].vouched_by = vec![key.clone()];
        let found = judge(&executed, &receipt);
        assert!(found[0].contains("was not reused"), "{found:?}");

        let mut twice = vouched.clone();
        twice.steps[0].vouched_by = vec![key.clone(), key.clone()];
        let found = judge(&twice, &receipt);
        assert!(found[0].contains("named more than once"), "{found:?}");

        let mut shouted = vouched.clone();
        shouted.steps[0].vouched_by = vec![format!("ed25519:{}", key[8..].to_uppercase())];
        let found = judge(&shouted, &receipt);
        assert!(found[0].contains("is not a signer key"), "{found:?}");
    }

    #[test]
    fn a_manifest_must_carry_its_receipts_flow_and_limitations() {
        let mut body = identity("a", "out");
        body.limitations = vec!["inline output trimmed".into()];
        let receipt = Receipt::seal(body, Observed::default());
        let manifest = coherent(&receipt);
        assert_eq!(judge(&manifest, &receipt), Vec::<String>::new());

        let mut cleaner = manifest.clone();
        cleaner.limitations.clear();
        let found = judge(&cleaner, &receipt);
        assert!(
            found[0].contains("omits the receipt's limitation"),
            "{found:?}"
        );

        let mut renamed = manifest.clone();
        renamed.flow = "other".into();
        let found = judge(&renamed, &receipt);
        assert!(found[0].contains("belongs to flow `demo`"), "{found:?}");
    }

    #[test]
    fn a_receipt_must_be_authentic_and_point_at_this_runs_dependencies() {
        let mut body = identity("a", "out");
        body.deps = vec![DepRef {
            step_id: "elsewhere".into(),
            receipt_id: "sha256:y".into(),
        }];
        let receipt = Receipt::seal(body, Observed::default());
        let found = judge(&coherent(&receipt), &receipt);
        assert!(found[0].contains("references `elsewhere`"), "{found:?}");

        let mut edited = Receipt::seal(identity("a", "out"), Observed::default());
        let manifest = coherent(&edited);
        edited.identity.command = "echo other".into();
        let found = judge(&manifest, &edited);
        assert!(found[0].contains("does not match its id"), "{found:?}");
    }

    /// A run whose final receipt `held` came after one attempt `earlier`,
    /// with each attempt's observation and spend.
    fn retried(held: &Receipt, earlier: &Receipt) -> (RunManifest, BTreeMap<String, Observed>) {
        let mut manifest = coherent(held);
        let paid = Observed {
            metrics: Some(Metrics {
                cost_micro_usd: Some(7),
                ..Metrics::default()
            }),
            ..Observed::default()
        };
        let paid_digest = digest::bytes(b"paid");
        manifest.steps[0].attempts = vec![RunAttempt {
            receipt_id: earlier.receipt_id.clone(),
            observed_digest: paid_digest.clone(),
        }];
        manifest.spend.add_executed(paid.metrics);
        let observations = BTreeMap::from([
            (
                manifest.steps[0].observed_digest.clone(),
                Observed::default(),
            ),
            (paid_digest, paid),
        ]);
        (manifest, observations)
    }

    fn judge_all(
        manifest: &RunManifest,
        receipts: &[&Receipt],
        observations: &BTreeMap<String, Observed>,
    ) -> Vec<String> {
        let receipts = receipts
            .iter()
            .map(|receipt| (receipt.receipt_id.as_str(), *receipt))
            .collect();
        let observations = observations
            .iter()
            .map(|(digest, observed)| (digest.as_str(), observed.clone()))
            .collect();
        manifest.disagreements(&receipts, &observations)
    }

    #[test]
    fn earlier_attempts_are_judged_with_the_run() {
        let held = Receipt::seal(identity("a", "second"), Observed::default());
        let mut failed = identity("a", "first");
        failed.status = StepStatus::Failed;
        let failed = Receipt::seal(failed, Observed::default());
        let (manifest, observations) = retried(&held, &failed);
        assert_eq!(
            judge_all(&manifest, &[&held, &failed], &observations),
            Vec::<String>::new()
        );

        let found = judge_all(&manifest, &[&held], &observations);
        assert!(found[0].contains("a attempt 1: receipt"), "{found:?}");

        // What an attempt spent is part of what the run spent.
        let mut cheaper = manifest.clone();
        cheaper.spend = coherent(&held).spend;
        let found = judge_all(&cheaper, &[&held, &failed], &observations);
        assert!(found[0].contains("run spend disagrees"), "{found:?}");

        // An attempt that held would have ended the step there.
        let (manifest, observations) = retried(&held, &held);
        let found = judge_all(&manifest, &[&held], &observations);
        assert!(
            found[0].contains("it held, so the step would not have been tried again"),
            "{found:?}"
        );

        // A reused receipt was not attempted in this run at all.
        let (mut manifest, observations) = retried(&held, &failed);
        manifest.steps[0].cached = true;
        manifest.spend = Spend::default();
        manifest.spend.add_reused(Observed::default().metrics);
        manifest
            .spend
            .add_executed(observations[&digest::bytes(b"paid")].metrics);
        let found = judge_all(&manifest, &[&held, &failed], &observations);
        assert!(
            found[0].contains("reused receipt lists attempts"),
            "{found:?}"
        );

        let mut other = identity("b", "first");
        other.status = StepStatus::Failed;
        let other = Receipt::seal(other, Observed::default());
        let (manifest, observations) = retried(&held, &other);
        let found = judge_all(&manifest, &[&held, &other], &observations);
        assert!(
            found[0].contains("receipt belongs to step `b`"),
            "{found:?}"
        );
    }

    fn candidate(name: &str, stdout: &str, status: StepStatus) -> Receipt {
        let mut body = identity("a", stdout);
        body.status = status;
        body.candidate = Some(name.into());
        Receipt::seal(body, Observed::default())
    }

    /// A run that kept `kept` over `other`, each having reported a cost.
    fn contest(
        kept: &Receipt,
        kept_cost: u64,
        other: &Receipt,
        other_cost: u64,
    ) -> (RunManifest, BTreeMap<String, Observed>) {
        let mut manifest = coherent(kept);
        let costing = |cost| Observed {
            metrics: Some(Metrics {
                cost_micro_usd: Some(cost),
                ..Metrics::default()
            }),
            ..Observed::default()
        };
        let other_digest = digest::bytes(b"other");
        manifest.steps[0].candidates = vec![RunCandidate {
            name: other.identity.candidate.clone().unwrap(),
            receipt_id: other.receipt_id.clone(),
            observed_digest: other_digest.clone(),
            attempts: Vec::new(),
        }];
        manifest.spend = Spend::default();
        manifest.spend.add_executed(costing(kept_cost).metrics);
        manifest.spend.add_executed(costing(other_cost).metrics);
        let observations = BTreeMap::from([
            (
                manifest.steps[0].observed_digest.clone(),
                costing(kept_cost),
            ),
            (other_digest, costing(other_cost)),
        ]);
        (manifest, observations)
    }

    #[test]
    fn the_kept_candidate_must_follow_from_the_others() {
        let cheap = candidate("cheap", "42", StepStatus::Ok);
        let pricey = candidate("pricey", "42 too", StepStatus::Ok);
        let wrong = candidate("wrong", "41", StepStatus::Failed);

        let (manifest, observations) = contest(&cheap, 1, &pricey, 5);
        assert_eq!(
            judge_all(&manifest, &[&cheap, &pricey], &observations),
            Vec::<String>::new()
        );
        // Keeping the dearer of two that held, or one that failed over one
        // that held, is not what the rule chooses.
        let (manifest, observations) = contest(&pricey, 5, &cheap, 1);
        let found = judge_all(&manifest, &[&cheap, &pricey], &observations);
        assert!(
            found[0].contains("candidate cheap: it ranks above"),
            "{found:?}"
        );
        let (manifest, observations) = contest(&wrong, 0, &cheap, 1);
        let found = judge_all(&manifest, &[&cheap, &wrong], &observations);
        assert!(
            found[0].contains("candidate cheap: it ranks above"),
            "{found:?}"
        );

        // A loser's receipt must be that candidate's.
        let (mut manifest, observations) = contest(&cheap, 1, &pricey, 5);
        manifest.steps[0].candidates[0].name = "other".into();
        let found = judge_all(&manifest, &[&cheap, &pricey], &observations);
        assert!(
            found[0].contains("receipt is not that candidate's"),
            "{found:?}"
        );

        // A kept receipt that names no candidate cannot have rivals.
        let plain = Receipt::seal(identity("a", "plain"), Observed::default());
        let (manifest, observations) = contest(&plain, 1, &pricey, 5);
        let found = judge_all(&manifest, &[&plain, &pricey], &observations);
        assert!(found[0].contains("receipt names none"), "{found:?}");

        // Nor can two entries be the same candidate.
        let again = candidate("cheap", "42 again", StepStatus::Ok);
        let (manifest, observations) = contest(&cheap, 1, &again, 5);
        let found = judge_all(&manifest, &[&cheap, &again], &observations);
        assert!(found[0].contains("appears more than once"), "{found:?}");
    }
}
