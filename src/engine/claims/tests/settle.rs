//! Tests of `settle` and `proof_command`: when a step's claims verify, are
//! refuted, or stay unverified.

use crate::core::receipt::{ClaimStatus, StepStatus};

use super::super::{proof_command, settle, Proof, ProofOutcome};
use super::{proof, statuses, step};

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
