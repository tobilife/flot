//! Comparing two runs by receipt identity, and saying why a step's receipt
//! changed. Pure: it reads manifests and the receipts it is given.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::core::digest;
use crate::core::receipt::{FileDigest, Partial, Receipt, ReceiptIdentity, RunManifest, Verdict};

/// How many files one reason names before it only counts the rest.
const NAMED_FILES: usize = 5;

#[derive(Debug, Clone, Serialize)]
pub struct StepDiff {
    pub step_id: String,
    pub change: String,
    pub left: Option<String>,
    pub right: Option<String>,
    /// What differs between the two receipts of a changed step, read from
    /// their bodies; empty for a step only one run has.
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunDiff {
    pub left: String,
    pub right: String,
    pub identical: bool,
    pub same_flow: bool,
    pub left_verdict: Verdict,
    pub right_verdict: Verdict,
    /// How many steps each run selected, when it did not run them all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_partial: Option<Partial>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_partial: Option<Partial>,
    pub steps: Vec<StepDiff>,
}

/// Compare two runs by receipt identity. Equal receipts mean the same work over
/// the same inputs reached the same verdict, regardless of timing. `receipts`
/// maps receipt id to receipt; a changed step whose receipts are both there
/// says why it changed.
pub fn diff(
    left: &RunManifest,
    right: &RunManifest,
    receipts: &BTreeMap<String, Receipt>,
) -> RunDiff {
    // A step one run of the same flow lacks was left out of a partial run,
    // not added to the flow or removed from it.
    let missing = |run: &RunManifest, side: &str, otherwise: &str| -> String {
        if run.partial.is_some() && left.flow_digest == right.flow_digest {
            format!("not selected in {side}")
        } else {
            otherwise.into()
        }
    };
    let mut steps = Vec::new();
    for step in &left.steps {
        match right.step(&step.step_id) {
            None => steps.push(StepDiff {
                step_id: step.step_id.clone(),
                change: missing(right, "right", "removed"),
                left: Some(digest::short(&step.receipt_id)),
                right: None,
                reasons: Vec::new(),
            }),
            Some(other) if other.receipt_id != step.receipt_id => steps.push(StepDiff {
                step_id: step.step_id.clone(),
                change: if other.status != step.status {
                    format!("status {} -> {}", step.status.label(), other.status.label())
                } else {
                    "receipt changed".into()
                },
                left: Some(digest::short(&step.receipt_id)),
                right: Some(digest::short(&other.receipt_id)),
                reasons: match (
                    receipts.get(&step.receipt_id),
                    receipts.get(&other.receipt_id),
                ) {
                    (Some(before), Some(after)) => why(&before.identity, &after.identity),
                    _ => vec!["a receipt is not in this store, so why cannot be told".into()],
                },
            }),
            Some(_) => {}
        }
    }
    for step in &right.steps {
        if left.step(&step.step_id).is_none() {
            steps.push(StepDiff {
                step_id: step.step_id.clone(),
                change: missing(left, "left", "added"),
                left: None,
                right: Some(digest::short(&step.receipt_id)),
                reasons: Vec::new(),
            });
        }
    }
    steps.sort_by(|a, b| a.step_id.cmp(&b.step_id));
    RunDiff {
        identical: left.root_digest == right.root_digest && steps.is_empty(),
        same_flow: left.flow_digest == right.flow_digest,
        left: left.run_id.clone(),
        right: right.run_id.clone(),
        left_verdict: left.verdict,
        right_verdict: right.verdict,
        left_partial: left.partial,
        right_partial: right.partial,
        steps,
    }
}

/// What makes two receipts of one step differ. A different key means the
/// work was not the same: its command, the files it or its proof read, its
/// dependencies' receipts, the flot that derived it, its claims, or the rest
/// of its definition. The same key means the same work came out otherwise.
fn why(left: &ReceiptIdentity, right: &ReceiptIdentity) -> Vec<String> {
    let mut why = Vec::new();
    if left.step_key == right.step_key {
        // What matters most first: whether it succeeded and what its claims
        // came to; output that prints a time differs on every run.
        let mut differs = Vec::new();
        if left.exit_code != right.exit_code {
            let code = |code: Option<i32>| code.map_or("none".into(), |code| code.to_string());
            differs.push(format!(
                "exit {} -> {}",
                code(left.exit_code),
                code(right.exit_code)
            ));
        }
        if left.claims != right.claims {
            differs.push("claim results".into());
        }
        if left.outputs != right.outputs {
            differs.push("outputs".into());
        }
        if left.changed != right.changed {
            differs.push("other files it changed".into());
        }
        if left.stdout_digest != right.stdout_digest {
            differs.push("stdout".into());
        }
        if left.stderr_digest != right.stderr_digest {
            differs.push("stderr".into());
        }
        why.push(if differs.is_empty() {
            "same key, and the receipts differ otherwise".into()
        } else {
            format!("same key, different result: {}", differs.join(", "))
        });
        return why;
    }
    if left.command != right.command {
        why.push("command changed".into());
    }
    why.extend(files("inputs", &left.inputs, &right.inputs));
    why.extend(files(
        "proof inputs",
        &left.verify_inputs,
        &right.verify_inputs,
    ));
    let deps = |identity: &ReceiptIdentity| -> BTreeMap<String, String> {
        identity
            .deps
            .iter()
            .map(|dep| (dep.step_id.clone(), dep.receipt_id.clone()))
            .collect()
    };
    let (before, after) = (deps(left), deps(right));
    let changed: Vec<&str> = before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|step| before.get(*step) != after.get(*step))
        .map(String::as_str)
        .collect();
    if !changed.is_empty() {
        why.push(format!("dependencies: {}", changed.join(", ")));
    }
    if left.tool != right.tool {
        why.push(format!(
            "derived by flot {} -> {}",
            left.tool.version, right.tool.version
        ));
    }
    let claimed = |identity: &ReceiptIdentity| -> Vec<(String, Option<String>)> {
        identity
            .claims
            .iter()
            .map(|claim| (claim.text.clone(), claim.verify_command.clone()))
            .collect()
    };
    if claimed(left) != claimed(right) {
        why.push("claims or their proof command changed".into());
    }
    if why.is_empty() {
        why.push("the rest of the step's definition changed (its env, outputs or settings)".into());
    }
    why
}

/// The files of one kind that differ, named up to [`NAMED_FILES`].
fn files(kind: &str, left: &[FileDigest], right: &[FileDigest]) -> Option<String> {
    let index = |files: &[FileDigest]| -> BTreeMap<String, (String, bool)> {
        files
            .iter()
            .map(|file| (file.path.clone(), (file.digest.clone(), file.executable)))
            .collect()
    };
    let (before, after) = (index(left), index(right));
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let differ: Vec<String> = paths
        .into_iter()
        .filter_map(|path| match (before.get(path), after.get(path)) {
            (Some(was), Some(is)) if was != is => Some(format!("{path} changed")),
            (Some(_), None) => Some(format!("{path} removed")),
            (None, Some(_)) => Some(format!("{path} added")),
            _ => None,
        })
        .collect();
    if differ.is_empty() {
        return None;
    }
    let named = differ.len().min(NAMED_FILES);
    let mut text = format!("{kind}: {}", differ[..named].join(", "));
    if differ.len() > named {
        text.push_str(&format!(" and {} more", differ.len() - named));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::receipt::{
        ClaimCounts, ClaimRecord, ClaimStatus, DepRef, Observed, RunStep, StepStatus, ToolIdentity,
    };

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
                "inputs: a.rs removed, b.rs removed, f0 added, f1 added, f2 added and 6 more"
                    .into(),
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
}
