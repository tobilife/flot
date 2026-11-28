//! Tests of the reasons a changed step's receipts give for the change.

use crate::core::digest;
use crate::core::receipt::{ClaimRecord, ClaimStatus, DepRef};

use super::super::reasons::why;
use super::{file, identity};

#[test]
fn a_changed_step_says_which_files_and_dependencies_changed() {
    let mut before = identity("sha256:1");
    before.inputs = vec![file("a.rs", "1"), file("b.rs", "1"), file("gone.rs", "1")];
    before.verify_inputs = vec![file("check.sh", "1")];
    before.deps = vec![
        DepRef {
            step_id: "fmt".into(),
            receipt_id: "sha256:f1".into(),
        },
        DepRef {
            step_id: "probe".into(),
            receipt_id: "sha256:p".into(),
        },
    ];
    let mut after = before.clone();
    after.step_key = "sha256:2".into();
    after.inputs = vec![file("a.rs", "2"), file("b.rs", "1"), file("new.rs", "1")];
    after.deps[0].receipt_id = "sha256:f2".into();
    assert_eq!(
        why(&before, &after),
        vec![
            "inputs: a.rs changed, gone.rs removed, new.rs added",
            "dependencies: fmt",
        ]
    );

    // An executable bit counts; past five files the rest are counted.
    let mut after = before.clone();
    after.step_key = "sha256:2".into();
    after.command = "make all".into();
    after.verify_inputs[0].executable = true;
    after.inputs = (0..8).map(|n| file(&format!("f{n}"), "1")).collect();
    after.tool.version = "9.9.9".into();
    assert_eq!(
        why(&before, &after),
        vec![
            "command changed".to_string(),
            "inputs: a.rs removed, b.rs removed, f0 added, f1 added, f2 added and 6 more".into(),
            "proof inputs: check.sh changed".into(),
            format!("derived by flot {} -> 9.9.9", before.tool.version),
        ]
    );

    // Nothing the receipt records: the rest of the definition did it.
    let mut after = before.clone();
    after.step_key = "sha256:2".into();
    assert_eq!(
        why(&before, &after),
        vec!["the rest of the step's definition changed (its env, outputs or settings)"]
    );
}

#[test]
fn the_same_work_coming_out_otherwise_is_named_as_such() {
    let before = identity("sha256:1");
    let mut after = before.clone();
    after.exit_code = Some(1);
    after.stderr_digest = digest::bytes(b"boom");
    assert_eq!(
        why(&before, &after),
        vec!["same key, different result: exit 0 -> 1, stderr"]
    );

    let mut after = before.clone();
    after.stdout_digest = digest::bytes(b"other");
    after.outputs = vec![file("out.txt", "1")];
    after.changed.insert("notes.md".into(), None);
    after.claims = vec![claim("it works", ClaimStatus::Refuted)];
    assert_eq!(
        why(&before, &after),
        vec!["same key, different result: claim results, outputs, other files it changed, stdout"]
    );
    let mut after = before.clone();
    after.exit_code = None;
    after.limitations = vec!["something".into()];
    assert_eq!(
        why(&before, &after),
        vec!["same key, different result: exit 0 -> none"]
    );
    after.exit_code = Some(0);
    assert_eq!(
        why(&before, &after),
        vec!["same key, and the receipts differ otherwise"]
    );
}

fn claim(text: &str, status: ClaimStatus) -> ClaimRecord {
    ClaimRecord {
        text: text.into(),
        status,
        verify_command: Some("true".into()),
        verify_exit: None,
        verify_output_digest: None,
        reason: None,
        before: None,
        held: None,
        mutants: None,
        differed: None,
    }
}

#[test]
fn a_changed_claim_or_proof_command_is_named() {
    let mut before = identity("sha256:1");
    before.claims = vec![claim("it works", ClaimStatus::Verified)];
    let mut after = before.clone();
    after.step_key = "sha256:2".into();
    after.claims[0].verify_command = Some("make check".into());
    assert_eq!(
        why(&before, &after),
        vec!["claims or their proof command changed"]
    );
    // How a claim came out is a result, not a change to the work.
    after.claims[0].verify_command = Some("true".into());
    after.claims[0].status = ClaimStatus::Refuted;
    assert_eq!(
        why(&before, &after),
        vec!["the rest of the step's definition changed (its env, outputs or settings)"]
    );
}
