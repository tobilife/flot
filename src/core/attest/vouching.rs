//! Who vouched for the steps a run reused, as far as a reader can confirm
//! it, and how a report names them.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::core::receipt::RunManifest;

use super::{check, fingerprint, Attestation};

/// The receipts signers vouched for, as far as a reader can check: each step
/// of each run it holds, under every key whose attestation of that run holds.
///
/// A run records who vouched for a step it reused, but that record is only
/// the run's word. The run CI executes is unsigned, so nothing covers it, and
/// an imported run can name any key. Only a signature by the key itself over
/// a run that kept the same receipt for the step confirms it.
#[derive(Debug, Clone, Default)]
pub struct Endorsements {
    /// Signer key, step id, receipt id.
    signed: BTreeSet<(String, String, String)>,
}

impl Endorsements {
    /// Count the steps of `manifest` as endorsed by each key in
    /// `attestations` whose signature over it holds. An attestation that does
    /// not hold endorses nothing.
    pub fn add(&mut self, attestations: &[Attestation], manifest: &RunManifest) {
        for attestation in attestations {
            if check(attestation, manifest).is_err() {
                continue;
            }
            for step in &manifest.steps {
                self.signed.insert((
                    attestation.key.clone(),
                    step.step_id.clone(),
                    step.receipt_id.clone(),
                ));
            }
        }
    }

    /// Whether `key` signed a run that kept `receipt_id` for `step_id`.
    pub fn endorse(&self, key: &str, step_id: &str, receipt_id: &str) -> bool {
        self.signed
            .contains(&(key.to_string(), step_id.to_string(), receipt_id.to_string()))
    }
}

/// A signer a run names for a step it reused from an imported run, judged by
/// a reader. Whether to believe the signer is the reader's decision, and may
/// change, so it is judged when the run is read, never recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Voucher {
    pub key: String,
    /// Whether the reader holds a run this key signed that kept the same
    /// receipt for the step.
    pub confirmed: bool,
    /// The name the reader trusts the key under, when it is confirmed.
    pub trusted_as: Option<String>,
}

impl Voucher {
    /// How a report names it: by its trusted name only when it is confirmed
    /// and trusted, so neither a signer named without a signature nor a
    /// revoked one ever reads as one this reader believes.
    pub fn label(&self) -> String {
        match (self.confirmed, &self.trusted_as) {
            (false, _) => format!("{} (not confirmed here)", fingerprint(&self.key)),
            (true, None) => format!("{} (not trusted here)", fingerprint(&self.key)),
            (true, Some(name)) => name.clone(),
        }
    }
}

/// A step a run reused from an imported run, and who vouched for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VouchedStep {
    pub step_id: String,
    pub vouched_by: Vec<Voucher>,
}

/// The steps of `manifest` that name who vouched for them, in run order,
/// with each signer confirmed by `endorsements` and named by `trusted`.
pub fn vouched(
    manifest: &RunManifest,
    trusted: &BTreeMap<String, String>,
    endorsements: &Endorsements,
) -> Vec<VouchedStep> {
    manifest
        .steps
        .iter()
        .filter(|step| !step.vouched_by.is_empty())
        .map(|step| VouchedStep {
            step_id: step.step_id.clone(),
            vouched_by: step
                .vouched_by
                .iter()
                .map(|key| {
                    let confirmed = endorsements.endorse(key, &step.step_id, &step.receipt_id);
                    Voucher {
                        key: key.clone(),
                        confirmed,
                        trusted_as: if confirmed {
                            trusted.get(key).cloned()
                        } else {
                            None
                        },
                    }
                })
                .collect(),
        })
        .collect()
}
