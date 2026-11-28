//! Tests of the verdict rule, the run root and the exit code of each verdict.

use crate::core::receipt::{verdict_for, ClaimCounts, RunManifest, RunStep, StepStatus, Verdict};

fn run_step(step: &str, status: StepStatus, claims: ClaimCounts) -> RunStep {
    RunStep {
        observed_digest: String::new(),
        attempts: Vec::new(),
        candidates: Vec::new(),
        step_id: step.into(),
        receipt_id: "sha256:x".into(),
        status,
        cached: false,
        claims,
        vouched_by: Vec::new(),
    }
}

#[test]
fn failure_outranks_unverified_claims() {
    let steps = vec![
        run_step("a", StepStatus::Failed, ClaimCounts::default()),
        run_step(
            "b",
            StepStatus::Ok,
            ClaimCounts {
                unverified: 1,
                ..ClaimCounts::default()
            },
        ),
    ];
    let claims = ClaimCounts {
        unverified: 1,
        ..ClaimCounts::default()
    };
    assert_eq!(verdict_for(&steps, claims), Verdict::Failed);
}

#[test]
fn a_refuted_claim_fails_the_run_even_when_every_step_exited_zero() {
    let claims = ClaimCounts {
        refuted: 1,
        verified: 3,
        unverified: 0,
    };
    let steps = vec![run_step("a", StepStatus::Ok, claims)];
    assert_eq!(verdict_for(&steps, claims), Verdict::Failed);
}

#[test]
fn unproven_claims_never_report_passed() {
    let claims = ClaimCounts {
        unverified: 1,
        verified: 1,
        refuted: 0,
    };
    let steps = vec![run_step("a", StepStatus::Ok, claims)];
    assert_eq!(verdict_for(&steps, claims), Verdict::Unverified);
}

#[test]
fn a_claim_free_run_that_succeeds_passes() {
    let steps = vec![run_step("a", StepStatus::Ok, ClaimCounts::default())];
    assert_eq!(verdict_for(&steps, ClaimCounts::default()), Verdict::Passed);
}

#[test]
fn run_root_is_order_sensitive() {
    let a = ("a".to_string(), "sha256:1".to_string());
    let b = ("b".to_string(), "sha256:2".to_string());
    assert_ne!(
        RunManifest::root_of(&[a.clone(), b.clone()]),
        RunManifest::root_of(&[b, a])
    );
}

#[test]
fn verdict_exit_codes_are_distinct() {
    assert_eq!(Verdict::Passed.exit_code(), 0);
    assert_eq!(Verdict::Failed.exit_code(), 1);
    assert_eq!(Verdict::Unverified.exit_code(), 2);
}
