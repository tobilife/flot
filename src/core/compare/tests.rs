//! Tests of comparing two runs, with the helpers the comparison tests share.

use super::*;
use crate::core::receipt::{
    ClaimCounts, FileDigest, Observed, ReceiptIdentity, RunStep, StepStatus, ToolIdentity,
};

mod reasons;

fn manifest(run_id: &str, steps: Vec<RunStep>) -> RunManifest {
    let pairs: Vec<(String, String)> = steps
        .iter()
        .map(|step| (step.step_id.clone(), step.receipt_id.clone()))
        .collect();
    RunManifest {
        run_id: run_id.into(),
        flow: "demo".into(),
        flow_digest: "sha256:flow".into(),
        root_digest: RunManifest::root_of(&pairs),
        verdict: Verdict::Passed,
        partial: None,
        claims: ClaimCounts::default(),
        steps,
        limitations: vec![],
        max_parallel: 1,
        spend: crate::core::metrics::Spend::default(),
        max_cost_micro_usd: None,
        started_at_ms: 0,
        duration_ms: 0,
        flot_version: "test".into(),
        refusals: Vec::new(),
    }
}

fn step(id: &str, receipt: &str, status: StepStatus) -> RunStep {
    RunStep {
        observed_digest: String::new(),
        attempts: Vec::new(),
        candidates: Vec::new(),
        step_id: id.into(),
        receipt_id: receipt.into(),
        status,
        cached: false,
        claims: ClaimCounts::default(),
        vouched_by: Vec::new(),
    }
}

#[test]
fn identical_runs_report_identical() {
    let left = manifest("a", vec![step("x", "sha256:1", StepStatus::Ok)]);
    let right = manifest("b", vec![step("x", "sha256:1", StepStatus::Ok)]);
    let diff = diff(&left, &right, &BTreeMap::new());
    assert!(diff.identical);
    assert!(diff.steps.is_empty());
}

#[test]
fn a_changed_receipt_is_reported_per_step() {
    let left = manifest("a", vec![step("x", "sha256:1", StepStatus::Ok)]);
    let right = manifest("b", vec![step("x", "sha256:2", StepStatus::Ok)]);
    let diff = diff(&left, &right, &BTreeMap::new());
    assert!(!diff.identical);
    assert_eq!(diff.steps.len(), 1);
    assert_eq!(diff.steps[0].change, "receipt changed");
}

#[test]
fn a_status_change_is_named_in_the_diff() {
    let left = manifest("a", vec![step("x", "sha256:1", StepStatus::Ok)]);
    let right = manifest("b", vec![step("x", "sha256:2", StepStatus::Failed)]);
    assert_eq!(
        diff(&left, &right, &BTreeMap::new()).steps[0].change,
        "status ok -> failed"
    );
}

#[test]
fn added_and_removed_steps_are_both_reported() {
    let left = manifest("a", vec![step("gone", "sha256:1", StepStatus::Ok)]);
    let right = manifest("b", vec![step("fresh", "sha256:2", StepStatus::Ok)]);
    let diff = diff(&left, &right, &BTreeMap::new());
    let changes: Vec<&str> = diff.steps.iter().map(|s| s.change.as_str()).collect();
    assert_eq!(changes, vec!["added", "removed"]);
}

fn identity(key: &str) -> ReceiptIdentity {
    ReceiptIdentity {
        flow: "demo".into(),
        step_id: "x".into(),
        step_key: key.into(),
        command: "make".into(),
        status: StepStatus::Ok,
        exit_code: Some(0),
        stdout_digest: digest::bytes(b"out"),
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

fn file(path: &str, content: &str) -> FileDigest {
    FileDigest {
        path: path.into(),
        digest: digest::bytes(content.as_bytes()),
        executable: false,
    }
}

#[test]
fn diff_reads_the_reasons_from_the_receipts_it_is_given() {
    let mut before = identity("sha256:1");
    before.inputs = vec![file("a.rs", "1")];
    let mut after = before.clone();
    after.step_key = "sha256:2".into();
    after.inputs = vec![file("a.rs", "2")];
    let (before, after) = (
        Receipt::seal(before, Observed::default()),
        Receipt::seal(after, Observed::default()),
    );
    let left = manifest("a", vec![step("x", &before.receipt_id, StepStatus::Ok)]);
    let right = manifest("b", vec![step("x", &after.receipt_id, StepStatus::Ok)]);
    let mut receipts = BTreeMap::new();
    receipts.insert(before.receipt_id.clone(), before);
    assert_eq!(
        diff(&left, &right, &receipts).steps[0].reasons,
        vec!["a receipt is not in this store, so why cannot be told"]
    );
    receipts.insert(after.receipt_id.clone(), after);
    assert_eq!(
        diff(&left, &right, &receipts).steps[0].reasons,
        vec!["inputs: a.rs changed"]
    );
}

#[test]
fn a_step_a_partial_run_left_out_is_not_called_added_or_removed() {
    let both = vec![
        step("a", "sha256:1", StepStatus::Ok),
        step("b", "sha256:2", StepStatus::Ok),
    ];
    let mut only_a = manifest("p", vec![step("a", "sha256:1", StepStatus::Ok)]);
    only_a.partial = Some(Partial { selected: 1, of: 2 });
    let whole = manifest("w", both);
    let receipts = BTreeMap::new();
    let forward = diff(&only_a, &whole, &receipts);
    assert_eq!(forward.steps[0].change, "not selected in left");
    assert_eq!(forward.left_partial, Some(Partial { selected: 1, of: 2 }));
    assert_eq!(forward.right_partial, None);
    assert_eq!(
        diff(&whole, &only_a, &receipts).steps[0].change,
        "not selected in right"
    );
    // Another flow can have added the step for real.
    let mut other = whole.clone();
    other.flow_digest = "sha256:other".into();
    assert_eq!(diff(&only_a, &other, &receipts).steps[0].change, "added");
}
