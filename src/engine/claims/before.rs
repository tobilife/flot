//! The `fails_before` hold: a verified claim stands only when its proof fails
//! with the step's changes undone.

use crate::core::receipt::{Before, ClaimRecord, ClaimStatus};

use super::Proof;

/// What the proof did on the work with its changes undone, for a step that
/// declares `fails_before`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Red {
    /// There is no revision to undo the changes to.
    NoRevision,
    /// The check could not be made: the copy could not be made or its
    /// changes undone, or the proof could not start.
    Unchecked(String),
    /// The proof does not pass on a copy of the work as it is, so a failure
    /// there with the changes undone would show nothing.
    CopyFails {
        exit_code: Option<i32>,
        timed_out: bool,
    },
    /// The proof ran with the changes undone to `commit`, which the run
    /// names as `baseline`.
    Ran {
        baseline: String,
        commit: String,
        proof: Proof,
    },
}

/// Hold the claims `settle` verified to `fails_before`: one whose proof
/// does not fail with the changes undone stays unverified, since passing
/// shows nothing the change did. Other claims are left as they are.
pub fn hold_before(
    claims: Vec<ClaimRecord>,
    red: Red,
    limitations: &mut Vec<String>,
) -> Vec<ClaimRecord> {
    let (before, reason) = match red {
        Red::NoRevision => (
            None,
            Some("fails_before needs a revision to fail at: set proofs_from".to_string()),
        ),
        Red::Unchecked(error) => (
            None,
            Some(format!(
                "could not check that the proof fails before the change ({error})"
            )),
        ),
        Red::CopyFails {
            exit_code,
            timed_out,
        } => {
            let how = match (timed_out, exit_code) {
                (true, _) => "timed out".to_string(),
                (false, Some(code)) => format!("exited {code}"),
                (false, None) => "was killed by a signal".to_string(),
            };
            (
                None,
                Some(format!(
                    "the proof {how} on a copy of the work, so failing there before the change would show nothing"
                )),
            )
        }
        Red::Ran {
            baseline,
            commit,
            proof,
        } => {
            let reason = match proof.exit_code {
                _ if proof.timed_out => Some(format!(
                    "the proof timed out with the changes undone to {baseline}, which is not a failure"
                )),
                None => Some(format!(
                    "the proof was killed with the changes undone to {baseline}, which is not a failure"
                )),
                Some(0) => Some(format!(
                    "the proof passes with the changes undone to {baseline} too, so it shows nothing this change did"
                )),
                Some(_) => None,
            };
            let before = Before {
                commit,
                exit_code: proof.exit_code,
                output_digest: proof.output_digest,
            };
            (Some(before), reason)
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
                    before: before.clone(),
                    ..claim
                },
                Some(reason) => ClaimRecord {
                    status: ClaimStatus::Unverified,
                    reason: Some(reason.clone()),
                    before: before.clone(),
                    ..claim
                },
            }
        })
        .collect()
}
