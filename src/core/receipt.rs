//! Receipts and run manifests.
//!
//! A receipt is the only thing flot trusts. It splits cleanly in two:
//!
//! * `identity` — the deterministic facts of a step: what ran, over which
//!   inputs, after which dependencies, with what exit code, output digests and
//!   claim verdicts. The receipt id is the digest of exactly this body.
//! * `observed` — host and wall-clock context plus inline output previews.
//!   Never hashed, because two honest runs of the same work disagree on timing.
//!
//! That split is what makes `flot diff` meaningful: identical identity digests
//! mean two runs did the same work and reached the same verdict.

mod check;
mod claim;
mod identity;
mod manifest;
mod observed;
mod verdict;

use serde::{Deserialize, Serialize};

use crate::core::digest;

pub use claim::{Before, ClaimRecord, ClaimStatus, Differed, Held, Mutated};
pub use identity::{DepRef, FileDigest, ReceiptIdentity, SandboxRecord, StepStatus, ToolIdentity};
pub use manifest::{
    candidate_rank, sum_costs, Partial, Refusal, RunAttempt, RunCandidate, RunManifest, RunStep,
};
pub use observed::{Observed, OMITTED};
pub use verdict::{verdict_for, ClaimCounts, Verdict};

pub const FLOT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub receipt_id: String,
    pub identity: ReceiptIdentity,
    pub observed: Observed,
}

impl Receipt {
    pub fn seal(identity: ReceiptIdentity, observed: Observed) -> Self {
        Receipt {
            receipt_id: digest::canonical(&identity),
            identity,
            observed,
        }
    }

    /// Digest of the observation, as a manifest records it.
    pub fn observed_digest(&self) -> String {
        digest::canonical(&self.observed)
    }

    /// Recompute the receipt id from the hashed body.
    pub fn recomputed_id(&self) -> String {
        digest::canonical(&self.identity)
    }

    pub fn is_authentic(&self) -> bool {
        self.recomputed_id() == self.receipt_id
    }

    /// Whether a later run may reuse this receipt instead of executing again.
    ///
    /// Only settled outcomes qualify. A failure, timeout or cancellation might
    /// not repeat, and neither might a refuted or timed-out proof, so those are
    /// always retried; so is a failure `allow_failure` tolerated, which is `ok`
    /// without exiting zero. A claim with no verify command stays unverified
    /// however often it runs, so it does not block reuse.
    pub fn is_reusable(&self) -> bool {
        self.identity.status.is_ok()
            && self.identity.exit_code == Some(0)
            && self.identity.claims.iter().all(|claim| match claim.status {
                ClaimStatus::Verified => true,
                ClaimStatus::Unverified => claim.verify_command.is_none(),
                ClaimStatus::Refuted => false,
            })
    }

    /// Every store object this receipt references: output streams, produced
    /// files and proof output. A receipt is only whole while all of them exist,
    /// and they are what travels with it in a bundle.
    pub fn objects(&self) -> Vec<&str> {
        let mut objects = vec![
            self.identity.stdout_digest.as_str(),
            self.identity.stderr_digest.as_str(),
        ];
        objects.extend(
            self.identity
                .outputs
                .iter()
                .map(|file| file.digest.as_str()),
        );
        for claim in &self.identity.claims {
            objects.extend(claim.verify_output_digest.as_deref());
            objects.extend(
                claim
                    .before
                    .as_ref()
                    .map(|before| before.output_digest.as_str()),
            );
            objects.extend(
                claim
                    .held
                    .as_ref()
                    .and_then(|held| held.output_digest.as_deref()),
            );
            if let Some(mutants) = &claim.mutants {
                objects.push(mutants.output_digest.as_str());
                objects.extend(mutants.report_digest.as_deref());
            }
        }
        objects
    }

    pub fn claim_counts(&self) -> ClaimCounts {
        let mut counts = ClaimCounts::default();
        for claim in &self.identity.claims {
            match claim.status {
                ClaimStatus::Verified => counts.verified += 1,
                ClaimStatus::Unverified => counts.unverified += 1,
                ClaimStatus::Refuted => counts.refuted += 1,
            }
        }
        counts
    }
}

#[cfg(test)]
mod tests;
