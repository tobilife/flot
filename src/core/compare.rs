//! Comparing two runs by receipt identity. Pure: it reads manifests only.

use serde::Serialize;

use crate::core::digest;
use crate::core::receipt::{RunManifest, Verdict};

#[derive(Debug, Clone, Serialize)]
pub struct StepDiff {
    pub step_id: String,
    pub change: String,
    pub left: Option<String>,
    pub right: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunDiff {
    pub left: String,
    pub right: String,
    pub identical: bool,
    pub same_flow: bool,
    pub left_verdict: Verdict,
    pub right_verdict: Verdict,
    pub steps: Vec<StepDiff>,
}

/// Compare two runs by receipt identity. Equal receipts mean the same work over
/// the same inputs reached the same verdict, regardless of timing.
pub fn diff(left: &RunManifest, right: &RunManifest) -> RunDiff {
    let mut steps = Vec::new();
    for step in &left.steps {
        match right.step(&step.step_id) {
            None => steps.push(StepDiff {
                step_id: step.step_id.clone(),
                change: "removed".into(),
                left: Some(digest::short(&step.receipt_id)),
                right: None,
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
            }),
            Some(_) => {}
        }
    }
    for step in &right.steps {
        if left.step(&step.step_id).is_none() {
            steps.push(StepDiff {
                step_id: step.step_id.clone(),
                change: "added".into(),
                left: None,
                right: Some(digest::short(&step.receipt_id)),
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
        steps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::receipt::{ClaimCounts, RunStep, StepStatus};

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
        let diff = diff(&left, &right);
        assert!(diff.identical);
        assert!(diff.steps.is_empty());
    }

    #[test]
    fn a_changed_receipt_is_reported_per_step() {
        let left = manifest("a", vec![step("x", "sha256:1", StepStatus::Ok)]);
        let right = manifest("b", vec![step("x", "sha256:2", StepStatus::Ok)]);
        let diff = diff(&left, &right);
        assert!(!diff.identical);
        assert_eq!(diff.steps.len(), 1);
        assert_eq!(diff.steps[0].change, "receipt changed");
    }

    #[test]
    fn a_status_change_is_named_in_the_diff() {
        let left = manifest("a", vec![step("x", "sha256:1", StepStatus::Ok)]);
        let right = manifest("b", vec![step("x", "sha256:2", StepStatus::Failed)]);
        assert_eq!(diff(&left, &right).steps[0].change, "status ok -> failed");
    }

    #[test]
    fn added_and_removed_steps_are_both_reported() {
        let left = manifest("a", vec![step("gone", "sha256:1", StepStatus::Ok)]);
        let right = manifest("b", vec![step("fresh", "sha256:2", StepStatus::Ok)]);
        let diff = diff(&left, &right);
        let changes: Vec<&str> = diff.steps.iter().map(|s| s.change.as_str()).collect();
        assert_eq!(changes, vec!["added", "removed"]);
    }
}
