//! Tests of receipts: what their id covers, what they reference and when
//! they may be reused, with the helpers the receipt tests share.

use std::collections::BTreeMap;

use super::*;

mod golden;
mod verdict;

pub(super) fn identity(step: &str, stdout: &str) -> ReceiptIdentity {
    ReceiptIdentity {
        flow: "demo".into(),
        step_id: step.into(),
        step_key: "sha256:key".into(),
        command: "echo hi".into(),
        status: StepStatus::Ok,
        exit_code: Some(0),
        stdout_digest: digest::bytes(stdout.as_bytes()),
        stderr_digest: digest::bytes(b""),
        inputs: vec![],
        verify_inputs: vec![],
        outputs: vec![],
        changed: BTreeMap::new(),
        deps: vec![],
        claims: vec![],
        limitations: vec![],
        tool: ToolIdentity::default(),
        candidate: None,
        sandbox: None,
    }
}

#[test]
fn objects_cover_every_piece_of_evidence() {
    let proof = digest::bytes(b"proof");
    let mut body = identity("a", "hello");
    body.outputs.push(FileDigest {
        executable: false,
        path: "out.txt".into(),
        digest: digest::bytes(b"out"),
    });
    let before = digest::bytes(b"before");
    body.claims.push(ClaimRecord {
        verify_output_digest: Some(proof.clone()),
        before: Some(Before {
            commit: "abc".into(),
            exit_code: Some(1),
            output_digest: before.clone(),
        }),
        held: None,
        mutants: None,
        differed: None,
        ..claim(ClaimStatus::Verified, Some("true"))
    });
    let receipt = Receipt::seal(body, Observed::default());
    let objects = receipt.objects();
    assert!(objects.contains(&receipt.identity.stdout_digest.as_str()));
    assert!(objects.contains(&receipt.identity.stderr_digest.as_str()));
    assert!(objects.contains(&digest::bytes(b"out").as_str()));
    assert!(objects.contains(&proof.as_str()));
    assert!(objects.contains(&before.as_str()));
}

#[test]
fn receipt_id_ignores_timing_but_tracks_output() {
    let fast = Receipt::seal(
        identity("a", "hello"),
        Observed {
            duration_ms: 1,
            started_at_ms: 1000,
            ..Observed::default()
        },
    );
    let slow = Receipt::seal(
        identity("a", "hello"),
        Observed {
            duration_ms: 9999,
            started_at_ms: 777_777,
            ..Observed::default()
        },
    );
    assert_eq!(fast.receipt_id, slow.receipt_id);

    let other_output = Receipt::seal(identity("a", "goodbye"), Observed::default());
    assert_ne!(fast.receipt_id, other_output.receipt_id);
}

#[test]
fn tampering_with_the_hashed_body_is_detectable() {
    let mut receipt = Receipt::seal(identity("a", "hello"), Observed::default());
    assert!(receipt.is_authentic());
    receipt.identity.exit_code = Some(0);
    receipt.identity.status = StepStatus::Failed;
    assert!(!receipt.is_authentic());
}

/// A stdout preview of `inline`, marked cut or not, of an 11-byte stream.
fn previewing(inline: &str, truncated: bool) -> Observed {
    Observed {
        stdout_inline: inline.into(),
        stdout_truncated: truncated,
        stdout_bytes: 11,
        ..Observed::default()
    }
}

#[test]
fn a_preview_marked_cut_must_leave_out_at_least_one_byte() {
    let data = b"hello world";
    assert!(previewing("hello world", false).describes(data, b""));
    // One byte short of the stream, from its beginning or around the marker.
    assert!(previewing("hello worl", true).describes(data, b""));
    assert!(previewing(&format!("hello{OMITTED}world"), true).describes(data, b""));
    // The whole stream, said to be cut, claims output that was never left out.
    assert!(!previewing("hello world", true).describes(data, b""));
    assert!(!previewing(&format!("hello{OMITTED} world"), true).describes(data, b""));
}

#[test]
fn editing_only_observation_keeps_the_receipt_authentic() {
    let mut receipt = Receipt::seal(identity("a", "hello"), Observed::default());
    receipt.observed.duration_ms = 42;
    assert!(receipt.is_authentic());
}

fn claim(status: ClaimStatus, verify_command: Option<&str>) -> ClaimRecord {
    ClaimRecord {
        text: "holds".into(),
        status,
        verify_command: verify_command.map(str::to_string),
        verify_exit: None,
        verify_output_digest: None,
        reason: None,
        before: None,
        held: None,
        mutants: None,
        differed: None,
    }
}

fn with(status: StepStatus, claims: Vec<ClaimRecord>) -> Receipt {
    let mut body = identity("a", "out");
    body.status = status;
    body.claims = claims;
    Receipt::seal(body, Observed::default())
}

#[test]
fn only_settled_success_is_reusable() {
    assert!(with(StepStatus::Ok, vec![]).is_reusable());
    assert!(with(
        StepStatus::Ok,
        vec![claim(ClaimStatus::Verified, Some("true"))]
    )
    .is_reusable());
    for status in [
        StepStatus::Failed,
        StepStatus::Timeout,
        StepStatus::Cancelled,
    ] {
        assert!(!with(status, vec![]).is_reusable(), "{status:?}");
    }
    // `ok` under allow_failure without exiting zero is a failure kept
    // from failing the run, and is tried again like one.
    let mut tolerated = identity("a", "out");
    tolerated.exit_code = Some(1);
    assert!(!Receipt::seal(tolerated, Observed::default()).is_reusable());
}

#[test]
fn a_receipt_that_changed_nothing_has_the_bytes_it_always_had() {
    let quiet = Receipt::seal(identity("a", "out"), Observed::default());
    assert!(!serde_json::to_string(&quiet).unwrap().contains("changed"));
    let mut body = identity("a", "out");
    body.changed
        .insert("src/lib.txt".into(), Some(digest::bytes(b"new")));
    assert_ne!(
        Receipt::seal(body, Observed::default()).receipt_id,
        quiet.receipt_id
    );
}

#[test]
fn a_refuted_or_unfinished_proof_is_retried() {
    assert!(!with(
        StepStatus::Ok,
        vec![claim(ClaimStatus::Refuted, Some("false"))]
    )
    .is_reusable());
    // Unverified despite a verify command means the proof did not finish.
    assert!(!with(
        StepStatus::Ok,
        vec![claim(ClaimStatus::Unverified, Some("sleep 99"))]
    )
    .is_reusable());
}

#[test]
fn a_claim_without_any_proof_command_does_not_block_reuse() {
    assert!(with(StepStatus::Ok, vec![claim(ClaimStatus::Unverified, None)]).is_reusable());
}
