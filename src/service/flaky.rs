//! Steps and proofs that gave different results on the same inputs.
//!
//! A step's key names everything it was given: its command, its input
//! content, the receipts it depends on. Two receipts with one key that
//! disagree, one `ok` and one `failed`, or a proof `verified` once and
//! `refuted` once over the same files, mean the result depends on something
//! the flow does not declare: time, the network, the machine, chance. Such a
//! proof is not one. Only runs executed here are read; a run imported from
//! another host ran somewhere else.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use crate::core::digest;
use crate::core::receipt::{ClaimStatus, StepStatus};
use crate::store::Store;

#[derive(Debug, Clone, Default, Serialize)]
pub struct FlakyReport {
    pub runs_read: usize,
    /// Steps whose command succeeded and failed on the same key.
    pub steps: Vec<Flaky>,
    /// Proofs that held and were refuted over the same step and files.
    pub claims: Vec<Flaky>,
    pub unreadable: Vec<String>,
}

impl FlakyReport {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.claims.is_empty()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Flaky {
    pub step_id: String,
    /// The step key, shortened.
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim: Option<String>,
    pub outcomes: Vec<Outcome>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub outcome: &'static str,
    /// Runs whose evidence holds this outcome, newest first.
    pub runs: Vec<String>,
}

type Seen = BTreeMap<&'static str, Vec<String>>;

/// Read the newest `runs` runs executed here.
pub fn flaky(store: &Store, runs: usize) -> Result<FlakyReport> {
    let mut report = FlakyReport::default();
    // (step id, key) -> outcome -> runs; and the same with the claim.
    let mut steps: BTreeMap<(String, String), Seen> = BTreeMap::new();
    let mut claims: BTreeMap<(String, String, String), Seen> = BTreeMap::new();
    let local = store
        .list_runs()?
        .into_iter()
        .filter(|run_id| !store.is_imported(run_id));
    for run_id in local.take(runs) {
        let manifest = match store.resolve_run(&run_id) {
            Ok(manifest) => manifest,
            Err(error) => {
                report.unreadable.push(format!("{run_id}: {error:#}"));
                continue;
            }
        };
        report.runs_read += 1;
        let mut read: Vec<&str> = Vec::new();
        for (_, receipt_id, _) in manifest.evidence() {
            if read.contains(&receipt_id) {
                continue;
            }
            read.push(receipt_id);
            let Ok(receipt) = store.load_receipt(receipt_id) else {
                report
                    .unreadable
                    .push(format!("{run_id}: receipt {receipt_id}"));
                continue;
            };
            let identity = &receipt.identity;
            let step = (identity.step_id.clone(), identity.step_key.clone());
            let outcome = match identity.status {
                StepStatus::Ok => Some("ok"),
                StepStatus::Failed => Some("failed"),
                // Stopped or never started: the command did not decide.
                StepStatus::Timeout | StepStatus::Cancelled => None,
            };
            if let Some(outcome) = outcome {
                note(&mut steps, step.clone(), outcome, &run_id);
            }
            // The files the proof read belong to its key: the step's key
            // alone does not cover them.
            let proof_files = digest::canonical(&identity.verify_inputs);
            for claim in &identity.claims {
                let outcome = match claim.status {
                    ClaimStatus::Verified => "verified",
                    ClaimStatus::Refuted => "refuted",
                    ClaimStatus::Unverified => continue,
                };
                let proof = format!(
                    "{}\n{}\n{proof_files}",
                    claim.verify_command.as_deref().unwrap_or_default(),
                    step.1
                );
                note(
                    &mut claims,
                    (step.0.clone(), claim.text.clone(), proof),
                    outcome,
                    &run_id,
                );
            }
        }
    }
    report.steps = steps
        .into_iter()
        .filter(|(_, seen)| seen.len() > 1)
        .map(|((step_id, key), seen)| Flaky {
            step_id,
            key: digest::short(&key),
            claim: None,
            outcomes: outcomes(seen),
        })
        .collect();
    report.claims = claims
        .into_iter()
        .filter(|(_, seen)| seen.len() > 1)
        .map(|((step_id, claim, proof), seen)| Flaky {
            step_id,
            key: digest::short(proof.lines().nth(1).unwrap_or_default()),
            claim: Some(claim),
            outcomes: outcomes(seen),
        })
        .collect();
    Ok(report)
}

fn note<K: Ord>(seen: &mut BTreeMap<K, Seen>, key: K, outcome: &'static str, run_id: &str) {
    let runs = seen.entry(key).or_default().entry(outcome).or_default();
    if !runs.iter().any(|run| run == run_id) {
        runs.push(run_id.to_string());
    }
}

fn outcomes(seen: Seen) -> Vec<Outcome> {
    seen.into_iter()
        .map(|(outcome, runs)| Outcome { outcome, runs })
        .collect()
}
