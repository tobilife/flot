//! Comparing two runs by receipt identity, and saying why a step's receipt
//! changed. Pure: it reads manifests and the receipts it is given.

mod reasons;

use std::collections::BTreeMap;

use serde::Serialize;

use crate::core::digest;
use crate::core::receipt::{Partial, Receipt, RunManifest, Verdict};

use reasons::why;

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

#[cfg(test)]
mod tests;
