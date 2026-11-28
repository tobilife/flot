//! Tests of `hold_mutants`: a verified claim stands only when no mutant the
//! allow list does not allow survives its proof.

use crate::core::mutants::Survivor;
use crate::core::receipt::{ClaimStatus, Mutated, StepStatus};

use super::super::{hold_mutants, settle, Judged, Mutation, Proof, ProofOutcome};
use super::{proof, statuses, step};

#[test]
fn only_a_proof_no_mutant_survives_unallowed_holds() {
    let step = step("claims = [\"x\"]\nverify = \"true\"");
    let verified = || {
        settle(
            &step,
            StepStatus::Ok,
            false,
            ProofOutcome::Ran(proof(Some(0), false)),
            &mut Vec::new(),
        )
    };
    let survivor = |line: u32| Survivor {
        file: "src/a.rs".into(),
        mutation: format!("replace f{line} with ()"),
        code: format!("sha256:{line}"),
        line,
    };
    let judged = |survived: Vec<Survivor>, allowed: Vec<Survivor>| Judged {
        caught: 7,
        untested: 2,
        survived,
        allowed,
    };
    let ran = |exit_code: Option<i32>,
               timed_out: bool,
               judged: Result<Judged, String>,
               changed: Vec<String>| Mutation::Ran {
        command: "m".into(),
        commit: Some("abc123".into()),
        run: Proof {
            exit_code,
            timed_out,
            output_digest: "sha256:mutants".into(),
        },
        report_digest: Some("sha256:report".into()),
        judged,
        changed,
    };

    // Every survivor allowed: verified, with what was tested recorded.
    let mut limitations = Vec::new();
    let held = hold_mutants(
        "s",
        verified(),
        ran(
            Some(2),
            false,
            Ok(judged(vec![], vec![survivor(1)])),
            vec![],
        ),
        &mut limitations,
    );
    assert_eq!(statuses(&held), vec![ClaimStatus::Verified]);
    assert!(limitations.is_empty(), "{limitations:?}");
    assert_eq!(
        held[0].mutants,
        Some(Mutated {
            command: "m".into(),
            commit: Some("abc123".into()),
            exit_code: Some(2),
            output_digest: "sha256:mutants".into(),
            report_digest: Some("sha256:report".into()),
            caught: 7,
            untested: 2,
            survived: vec![],
            allowed: vec![survivor(1)],
        })
    );

    let read = "read `flot log s --mutants`";
    for (mutation, reason) in [
        (
            ran(Some(2), false, Ok(judged(vec![survivor(4)], vec![])), vec![]),
            "1 mutant survived the proof: src/a.rs:4: replace f4 with (); `flot show` names each as the allow list does".to_string(),
        ),
        (
            ran(
                Some(2),
                false,
                Ok(judged((1..=5).map(survivor).collect(), vec![])),
                vec![],
            ),
            "5 mutants survived the proof: src/a.rs:1: replace f1 with (); src/a.rs:2: replace f2 with (); src/a.rs:3: replace f3 with (); and 2 more;".to_string(),
        ),
        (
            ran(Some(0), true, Ok(judged(vec![], vec![])), vec![]),
            format!("the mutation run timed out; {read}"),
        ),
        (
            ran(None, false, Ok(judged(vec![], vec![])), vec![]),
            format!("the mutation run was stopped; {read}"),
        ),
        (
            ran(
                Some(0),
                false,
                Ok(judged(vec![], vec![])),
                vec!["out.txt".into(), "t.sh".into()],
            ),
            "the mutation run changed out.txt, t.sh, which the verdict judged".to_string(),
        ),
        (
            ran(Some(1), false, Err("no report".into()), vec![]),
            format!("the mutation report shows nothing: no report; {read}"),
        ),
        (
            Mutation::Unchecked("no scratch".into()),
            "could not test the proof against mutants (no scratch)".to_string(),
        ),
    ] {
        let mut limitations = Vec::new();
        let held = hold_mutants("s", verified(), mutation, &mut limitations);
        assert_eq!(statuses(&held), vec![ClaimStatus::Unverified], "{reason}");
        let said = held[0].reason.as_deref().unwrap();
        assert!(said.contains(&reason), "{said}");
        assert_eq!(limitations, [format!("{said}; claims stay unverified")]);
    }

    // What did not verify is left as it was.
    let refuted = settle(
        &step,
        StepStatus::Ok,
        false,
        ProofOutcome::Ran(proof(Some(1), false)),
        &mut Vec::new(),
    );
    let held = hold_mutants(
        "s",
        refuted.clone(),
        ran(
            Some(2),
            false,
            Ok(judged(vec![survivor(4)], vec![])),
            vec![],
        ),
        &mut Vec::new(),
    );
    assert_eq!(held, refuted);
}
