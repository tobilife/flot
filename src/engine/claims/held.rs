//! The `held` hold: a verified claim stands only when its proof also passes
//! with the step's held files as the revision has them.

use crate::core::receipt::{ClaimRecord, ClaimStatus, Held};

use super::Proof;

/// What the proof did with the files a step holds to the revision as the
/// revision has them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// There is no revision to hold the files to.
    NoRevision,
    /// The check could not be made: the files could not be read, the copy
    /// could not be made, or the proof could not start there.
    Unchecked(String),
    /// The files are here as the revision has them, `base`: the proof that
    /// passed already ran with them.
    Same { base: String },
    /// The proof ran on a copy of the work with `files` as `baseline` has
    /// them, which `base` digests.
    Ran {
        baseline: String,
        base: String,
        files: Vec<String>,
        proof: Proof,
    },
}

/// Hold the claims verified so far to the step's `held` files: one whose
/// proof does not also pass with them as the revision has them stays
/// unverified, since the work passes only proofs it rewrote or dropped.
/// Other claims are left as they are.
pub fn hold_held(
    step_id: &str,
    claims: Vec<ClaimRecord>,
    hold: Hold,
    limitations: &mut Vec<String>,
) -> Vec<ClaimRecord> {
    let (held, reason) = match hold {
        Hold::NoRevision => (
            None,
            Some("held files need a revision to hold them to: set proofs_from".to_string()),
        ),
        Hold::Unchecked(error) => (
            None,
            Some(format!(
                "could not prove the work with the held files as the revision has them ({error})"
            )),
        ),
        Hold::Same { base } => (
            Some(Held {
                base,
                files: Vec::new(),
                exit_code: None,
                output_digest: None,
            }),
            None,
        ),
        Hold::Ran {
            baseline,
            base,
            files,
            proof,
        } => {
            let how = match proof.exit_code {
                _ if proof.timed_out => Some("timed out".to_string()),
                None => Some("was killed".to_string()),
                Some(0) => None,
                Some(code) => Some(format!("exited {code}")),
            };
            let reason = how.map(|how| {
                format!(
                    "the proof {how} with {} as {baseline} has {}: the work passes only the versions it changed; read `flot log {step_id} --held`",
                    files.join(", "),
                    if files.len() == 1 { "it" } else { "them" }
                )
            });
            (
                Some(Held {
                    base,
                    files,
                    exit_code: proof.exit_code,
                    output_digest: Some(proof.output_digest),
                }),
                reason,
            )
        }
    };
    if let Some(reason) = &reason {
        limitations.push(format!("{reason}; claims stay unverified"));
    }
    claims
        .into_iter()
        .map(|claim| {
            if claim.status != ClaimStatus::Verified {
                return claim;
            }
            match &reason {
                None => ClaimRecord {
                    held: held.clone(),
                    ..claim
                },
                Some(reason) => ClaimRecord {
                    status: ClaimStatus::Unverified,
                    reason: Some(reason.clone()),
                    held: held.clone(),
                    ..claim
                },
            }
        })
        .collect()
}
