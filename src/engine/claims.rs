//! Claim settlement: the rule that makes flot more than a task runner.
//!
//! This module decides, and never executes. The runner runs the proof command
//! when `proof_command` asks for one and hands the outcome back to `settle`.
//! Keeping the decision pure means the only path to `verified` is readable in
//! one place: the step succeeded, it left its proof untouched, a proof command
//! ran, and it exited zero.

use crate::core::flow::Step;
use crate::core::receipt::{Before, ClaimRecord, ClaimStatus, Held, StepStatus};

/// What running a proof command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// Digest of the proof command's combined output.
    pub output_digest: String,
}

/// What happened when the runner went to prove a step's claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofOutcome {
    /// No proof was attempted: nothing to prove, or no command to prove it.
    NotRun,
    /// The step changed files its own proof reads, so the proof was not run:
    /// whatever it said would no longer be independent of the work it judges.
    Compromised {
        changed: Vec<String>,
    },
    Ran(Proof),
    /// The proof command could not be started at all.
    Failed(String),
    /// The proof differs from the revision the run checks proofs against,
    /// so it was not run. `claims` are the step's claims and any it made at
    /// that revision.
    Unanchored {
        baseline: String,
        reasons: Vec<String>,
        claims: Vec<String>,
    },
    /// The proof ran, but files it judged were different afterwards: the
    /// proof changed the step's outputs, or something changed its inputs
    /// while it ran. Whatever it concluded was about files that are gone.
    Disturbed {
        changed: Vec<String>,
    },
}

/// The proof command to run, when settling this step's claims needs one.
/// `tolerated` is a nonzero exit that `allow_failure` kept from failing the
/// run: the run goes on, but failed work proves nothing.
pub fn proof_command(step: &Step, status: StepStatus, tolerated: bool) -> Option<&str> {
    if step.claims.is_empty() || !status.is_ok() || tolerated {
        return None;
    }
    step.verify.as_deref()
}

/// Decide every claim of a step. `proof` is `NotRun` exactly when
/// `proof_command` returned no command.
pub fn settle(
    step: &Step,
    status: StepStatus,
    tolerated: bool,
    proof: ProofOutcome,
    limitations: &mut Vec<String>,
) -> Vec<ClaimRecord> {
    // Checked first: a claim dropped from the flow since the revision still
    // shows, as unproven.
    if let ProofOutcome::Unanchored {
        baseline,
        reasons,
        claims,
    } = &proof
    {
        limitations.push(format!(
            "the proof differs from {baseline} ({}); claims stay unverified and the proof was not run",
            reasons.join(", ")
        ));
        let reason = format!("the proof differs from {baseline}");
        return claims
            .iter()
            .map(|text| ClaimRecord {
                text: text.clone(),
                status: ClaimStatus::Unverified,
                verify_command: step.verify.clone(),
                verify_exit: None,
                verify_output_digest: None,
                reason: Some(reason.clone()),
                before: None,
                held: None,
            })
            .collect();
    }
    if step.claims.is_empty() {
        if step.verify.is_some() {
            limitations.push("a verify command is declared but the step claims nothing".into());
        }
        return Vec::new();
    }
    if !status.is_ok() {
        limitations.push("claims are unverified because the step did not succeed".into());
        return unverified(step, "step did not succeed");
    }
    if tolerated {
        limitations.push(
            "claims are unverified because the step failed; allow_failure keeps the run going but proves nothing".into(),
        );
        return unverified(step, "step failed under allow_failure");
    }
    let Some(command) = &step.verify else {
        limitations
            .push("claims are unverified: declare `verify` with a command that proves them".into());
        return unverified(step, "no verify command declared");
    };
    let proof = match proof {
        ProofOutcome::Ran(proof) => proof,
        ProofOutcome::Compromised { changed } => {
            limitations.push(format!(
                "the step changed its own proof ({}); claims stay unverified and the proof was not run",
                changed.join(", ")
            ));
            return unverified(step, "the step modified files its proof reads");
        }
        ProofOutcome::Failed(error) => {
            limitations.push(format!(
                "verify command could not run ({error}); claims stay unverified"
            ));
            return unverified(step, "verify command could not run");
        }
        ProofOutcome::Disturbed { changed } => {
            limitations.push(format!(
                "files the proof judged changed while it ran ({}); claims stay unverified",
                changed.join(", ")
            ));
            return unverified(step, "files changed while the proof ran");
        }
        ProofOutcome::NotRun | ProofOutcome::Unanchored { .. } => {
            unreachable!("a step with claims, a verify command and success always attempts a proof")
        }
    };

    let (status, reason) = if proof.timed_out {
        limitations.push("verify command hit its timeout budget; claims stay unverified".into());
        (
            ClaimStatus::Unverified,
            Some("verify command exceeded its timeout budget".to_string()),
        )
    } else if proof.exit_code == Some(0) {
        (ClaimStatus::Verified, None)
    } else if proof.exit_code.is_none() {
        // Killed from outside, not a judgement: refuting on it would let
        // anything that kills the proof fail the work.
        limitations.push("verify command was killed by a signal; claims stay unverified".into());
        (
            ClaimStatus::Unverified,
            Some("verify command was killed by a signal".to_string()),
        )
    } else {
        let exit = proof.exit_code.unwrap_or_default();
        limitations.push(format!("verify command exited {exit}; claims are refuted"));
        (
            ClaimStatus::Refuted,
            Some(format!("verify command exited {exit}")),
        )
    };

    step.claims
        .iter()
        .map(|text| ClaimRecord {
            text: text.clone(),
            status,
            verify_command: Some(command.clone()),
            verify_exit: proof.exit_code,
            verify_output_digest: Some(proof.output_digest.clone()),
            reason: reason.clone(),
            before: None,
            held: None,
        })
        .collect()
}

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

/// Claims of a step that never ran.
pub fn not_run(step: &Step, reason: &str) -> Vec<ClaimRecord> {
    unverified(step, reason)
}

fn unverified(step: &Step, reason: &str) -> Vec<ClaimRecord> {
    step.claims
        .iter()
        .map(|text| ClaimRecord {
            text: text.clone(),
            status: ClaimStatus::Unverified,
            verify_command: step.verify.clone(),
            verify_exit: None,
            verify_output_digest: None,
            reason: Some(reason.to_string()),
            before: None,
            held: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::flow::Flow;

    fn step(extra: &str) -> Step {
        let text =
            format!("[flow]\nname = \"c\"\n\n[[step]]\nid = \"s\"\nrun = \"true\"\n{extra}\n");
        Flow::parse(&text).unwrap().step("s").unwrap().clone()
    }

    fn proof(exit_code: Option<i32>, timed_out: bool) -> Proof {
        Proof {
            exit_code,
            timed_out,
            output_digest: "sha256:proof".into(),
        }
    }

    fn statuses(claims: &[ClaimRecord]) -> Vec<ClaimStatus> {
        claims.iter().map(|claim| claim.status).collect()
    }

    #[test]
    fn only_a_zero_exit_proof_after_success_verifies() {
        let step = step("claims = [\"x\", \"y\"]\nverify = \"true\"");
        assert_eq!(proof_command(&step, StepStatus::Ok, false), Some("true"));
        let mut limitations = Vec::new();
        let claims = settle(
            &step,
            StepStatus::Ok,
            false,
            ProofOutcome::Ran(proof(Some(0), false)),
            &mut limitations,
        );
        assert_eq!(statuses(&claims), vec![ClaimStatus::Verified; 2]);
        assert!(limitations.is_empty());
    }

    #[test]
    fn a_nonzero_proof_refutes() {
        let step = step("claims = [\"x\"]\nverify = \"false\"");
        let mut limitations = Vec::new();
        let claims = settle(
            &step,
            StepStatus::Ok,
            false,
            ProofOutcome::Ran(proof(Some(1), false)),
            &mut limitations,
        );
        assert_eq!(statuses(&claims), vec![ClaimStatus::Refuted]);
        assert_eq!(claims[0].verify_exit, Some(1));
    }

    #[test]
    fn a_timed_out_proof_neither_verifies_nor_refutes() {
        let step = step("claims = [\"x\"]\nverify = \"sleep 9\"");
        let mut limitations = Vec::new();
        let claims = settle(
            &step,
            StepStatus::Ok,
            false,
            ProofOutcome::Ran(proof(None, true)),
            &mut limitations,
        );
        assert_eq!(statuses(&claims), vec![ClaimStatus::Unverified]);
    }

    #[test]
    fn a_failed_step_runs_no_proof_and_verifies_nothing() {
        let step = step("claims = [\"x\"]\nverify = \"true\"");
        assert_eq!(proof_command(&step, StepStatus::Failed, false), None);
        let mut limitations = Vec::new();
        let claims = settle(
            &step,
            StepStatus::Failed,
            false,
            ProofOutcome::NotRun,
            &mut limitations,
        );
        assert_eq!(statuses(&claims), vec![ClaimStatus::Unverified]);
        assert_eq!(claims[0].reason.as_deref(), Some("step did not succeed"));
    }

    #[test]
    fn claims_without_a_proof_command_stay_unverified() {
        let step = step("claims = [\"x\"]");
        assert_eq!(proof_command(&step, StepStatus::Ok, false), None);
        let mut limitations = Vec::new();
        let claims = settle(
            &step,
            StepStatus::Ok,
            false,
            ProofOutcome::NotRun,
            &mut limitations,
        );
        assert_eq!(statuses(&claims), vec![ClaimStatus::Unverified]);
        assert_eq!(claims[0].verify_command, None);
    }

    #[test]
    fn a_proof_command_without_claims_is_reported_and_never_run() {
        let step = step("verify = \"true\"");
        assert_eq!(proof_command(&step, StepStatus::Ok, false), None);
        let mut limitations = Vec::new();
        assert!(settle(
            &step,
            StepStatus::Ok,
            false,
            ProofOutcome::NotRun,
            &mut limitations
        )
        .is_empty());
        assert_eq!(limitations.len(), 1);
    }

    #[test]
    fn a_step_that_rewrote_its_proof_verifies_nothing() {
        let step = step("claims = [\"x\"]\nverify = \"python3 check.py\"");
        let mut limitations = Vec::new();
        let claims = settle(
            &step,
            StepStatus::Ok,
            false,
            ProofOutcome::Compromised {
                changed: vec!["check.py".into()],
            },
            &mut limitations,
        );
        assert_eq!(statuses(&claims), vec![ClaimStatus::Unverified]);
        assert_eq!(
            claims[0].reason.as_deref(),
            Some("the step modified files its proof reads")
        );
        assert!(limitations[0].contains("check.py"), "{}", limitations[0]);
    }

    #[test]
    fn only_a_proof_that_fails_before_the_change_holds() {
        let step = step("claims = [\"x\"]\nverify = \"true\"\nfails_before = true");
        let verified = || {
            settle(
                &step,
                StepStatus::Ok,
                false,
                ProofOutcome::Ran(proof(Some(0), false)),
                &mut Vec::new(),
            )
        };
        let at = |exit_code: Option<i32>, timed_out: bool| Red::Ran {
            baseline: "main (abc)".into(),
            commit: "abc123".into(),
            proof: Proof {
                exit_code,
                timed_out,
                output_digest: "sha256:before".into(),
            },
        };

        let mut limitations = Vec::new();
        let held = hold_before(verified(), at(Some(1), false), &mut limitations);
        assert_eq!(statuses(&held), vec![ClaimStatus::Verified]);
        assert!(limitations.is_empty(), "{limitations:?}");
        let before = held[0].before.as_ref().unwrap();
        assert_eq!(
            (
                before.commit.as_str(),
                before.exit_code,
                before.output_digest.as_str()
            ),
            ("abc123", Some(1), "sha256:before")
        );
        assert_eq!(
            held[0].verify_output_digest.as_deref(),
            Some("sha256:proof")
        );

        for (red, reason) in [
            (
                at(Some(0), false),
                "passes with the changes undone to main (abc) too",
            ),
            (at(Some(1), true), "timed out with the changes undone"),
            (at(None, false), "was killed with the changes undone"),
            (Red::NoRevision, "set proofs_from"),
            (Red::Unchecked("no copy".into()), "(no copy)"),
            (
                Red::CopyFails {
                    exit_code: Some(2),
                    timed_out: false,
                },
                "exited 2 on a copy of the work",
            ),
            (
                Red::CopyFails {
                    exit_code: None,
                    timed_out: true,
                },
                "timed out on a copy of the work",
            ),
        ] {
            let mut limitations = Vec::new();
            let held = hold_before(verified(), red, &mut limitations);
            assert_eq!(statuses(&held), vec![ClaimStatus::Unverified], "{reason}");
            let said = held[0].reason.as_deref().unwrap();
            assert!(said.contains(reason), "{said}");
            assert!(limitations[0].ends_with("; claims stay unverified"));
        }

        // What did not verify is left as it was.
        let refuted = settle(
            &step,
            StepStatus::Ok,
            false,
            ProofOutcome::Ran(proof(Some(1), false)),
            &mut Vec::new(),
        );
        let held = hold_before(refuted.clone(), at(Some(1), false), &mut Vec::new());
        assert_eq!(held, refuted);
    }

    fn ran(exit_code: Option<i32>) -> ProofOutcome {
        ProofOutcome::Ran(Proof {
            exit_code,
            timed_out: false,
            output_digest: "sha256:x".into(),
        })
    }

    #[test]
    fn a_tolerated_failure_proves_nothing() {
        let step = step("claims = [\"x\"]\nverify = \"true\"\nallow_failure = true");
        assert_eq!(proof_command(&step, StepStatus::Ok, true), None);
        let mut limitations = Vec::new();
        let claims = settle(
            &step,
            StepStatus::Ok,
            true,
            ProofOutcome::NotRun,
            &mut limitations,
        );
        assert_eq!(statuses(&claims), vec![ClaimStatus::Unverified]);
    }

    #[test]
    fn a_proof_killed_by_a_signal_neither_verifies_nor_refutes() {
        let step = step("claims = [\"x\"]\nverify = \"true\"");
        let mut limitations = Vec::new();
        let claims = settle(&step, StepStatus::Ok, false, ran(None), &mut limitations);
        assert_eq!(statuses(&claims), vec![ClaimStatus::Unverified]);
    }

    #[test]
    fn a_proof_whose_files_moved_under_it_verifies_nothing() {
        let step = step("claims = [\"x\"]\nverify = \"true\"");
        let mut limitations = Vec::new();
        let claims = settle(
            &step,
            StepStatus::Ok,
            false,
            ProofOutcome::Disturbed {
                changed: vec!["out.txt".into()],
            },
            &mut limitations,
        );
        assert_eq!(statuses(&claims), vec![ClaimStatus::Unverified]);
        assert!(limitations[0].contains("out.txt"));
    }
}
