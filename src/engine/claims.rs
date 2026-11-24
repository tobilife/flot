//! Claim settlement: the rule that makes flot more than a task runner.
//!
//! This module decides, and never executes. The runner runs the proof command
//! when `proof_command` asks for one and hands the outcome back to `settle`.
//! Keeping the decision pure means the only path to `verified` is readable in
//! one place: the step succeeded, it left its proof untouched, a proof command
//! ran, and it exited zero.

use crate::core::flow::Step;
use crate::core::receipt::{ClaimRecord, ClaimStatus, StepStatus};

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
