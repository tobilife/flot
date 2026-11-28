//! The mutants hold: a verified claim stands only when no mutant the allow
//! list does not allow survives its proof.

use crate::core::mutants::Survivor;
use crate::core::receipt::{ClaimRecord, ClaimStatus, Mutated};

use super::Proof;

/// What testing a step's proof against mutants came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mutation {
    /// The mutation tester could not be run.
    Unchecked(String),
    /// It ran `command`, with `commit` as `$FLOT_PROOFS_FROM`, ending as
    /// `run` says.
    Ran {
        command: String,
        commit: Option<String>,
        run: Proof,
        /// Digest of the report it wrote, when it wrote one.
        report_digest: Option<String>,
        /// The report, judged against the allow list, or why it could not
        /// be read.
        judged: Result<Judged, String>,
        /// Files the verdict judged that the run changed.
        changed: Vec<String>,
    },
}

/// A complete report, its survivors split by the allow list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    pub caught: u32,
    pub untested: u32,
    pub survived: Vec<Survivor>,
    pub allowed: Vec<Survivor>,
}

/// Hold the claims verified so far to the mutants of the work: one whose
/// proof let a mutant survive that no entry of the allow list allows stays
/// unverified, since the proof cannot tell that change from the work. So
/// does one whose mutants could not all be tested. Other claims are left as
/// they are.
pub fn hold_mutants(
    step_id: &str,
    claims: Vec<ClaimRecord>,
    mutation: Mutation,
    limitations: &mut Vec<String>,
) -> Vec<ClaimRecord> {
    let read = format!("read `flot log {step_id} --mutants`");
    let (mutated, reason) = match mutation {
        Mutation::Unchecked(error) => (
            None,
            Some(format!(
                "could not test the proof against mutants ({error})"
            )),
        ),
        Mutation::Ran {
            command,
            commit,
            run,
            report_digest,
            judged,
            changed,
        } => {
            let reason = if run.timed_out {
                Some(format!("the mutation run timed out; {read}"))
            } else if run.exit_code.is_none() {
                Some(format!("the mutation run was stopped; {read}"))
            } else if !changed.is_empty() {
                Some(format!(
                    "the mutation run changed {}, which the verdict judged",
                    changed.join(", ")
                ))
            } else {
                match &judged {
                    Err(error) => Some(format!(
                        "the mutation report shows nothing: {error}; {read}"
                    )),
                    Ok(judged) if judged.survived.is_empty() => None,
                    Ok(judged) => {
                        let count = judged.survived.len();
                        let named: Vec<String> = judged
                            .survived
                            .iter()
                            .take(3)
                            .map(|survivor| {
                                format!(
                                    "{}:{}: {}",
                                    survivor.file, survivor.line, survivor.mutation
                                )
                            })
                            .collect();
                        Some(format!(
                            "{count} {} the proof: {}{}; `flot show` names each as the allow list does",
                            if count == 1 { "mutant survived" } else { "mutants survived" },
                            named.join("; "),
                            if count > 3 {
                                format!("; and {} more", count - 3)
                            } else {
                                String::new()
                            }
                        ))
                    }
                }
            };
            let judged = judged.unwrap_or(Judged {
                caught: 0,
                untested: 0,
                survived: Vec::new(),
                allowed: Vec::new(),
            });
            (
                Some(Mutated {
                    command,
                    commit,
                    exit_code: run.exit_code,
                    output_digest: run.output_digest,
                    report_digest,
                    caught: judged.caught,
                    untested: judged.untested,
                    survived: judged.survived,
                    allowed: judged.allowed,
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
                    mutants: mutated.clone(),
                    ..claim
                },
                Some(reason) => ClaimRecord {
                    status: ClaimStatus::Unverified,
                    reason: Some(reason.clone()),
                    mutants: mutated.clone(),
                    ..claim
                },
            }
        })
        .collect()
}
