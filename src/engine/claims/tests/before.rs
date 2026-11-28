//! Tests of `hold_before`: a verified claim stands only when its proof fails
//! with the changes undone.

use crate::core::receipt::{ClaimStatus, StepStatus};

use super::super::{hold_before, settle, Proof, ProofOutcome, Red};
use super::{proof, statuses, step};

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
