//! What every receipt a run names must be to belong to the run.

use crate::core::digest;
use crate::core::receipt::{ClaimStatus, Receipt, RunManifest};

impl RunManifest {
    /// What every receipt a run names must be: authentic, of this flow, with
    /// proof behind each verified claim, built on the receipts this run
    /// recorded for its dependencies.
    pub(super) fn belongs(&self, label: &str, receipt: &Receipt, found: &mut Vec<String>) {
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
