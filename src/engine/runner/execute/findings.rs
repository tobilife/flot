//! Judging a step by how its command ended and what it left: its status,
//! its limitations in the order found, and what it and its proofs spent.

use crate::core::flow::{ResolvedBudget, Step};
use crate::core::metrics::Metrics;
use crate::core::receipt::{ClaimStatus, StepStatus};
use crate::engine::exec;

use super::Findings;

impl Findings {
    /// The step's status from how its command ended: a limitation, after
    /// `limitations`, for each way it did not simply exit zero, and whether
    /// `allow_failure` let a failure pass. `reported` is what it reported
    /// spending.
    pub(super) fn ended(
        mut limitations: Vec<String>,
        step: &Step,
        budget: ResolvedBudget,
        outcome: &exec::Outcome,
        reported: (Option<Metrics>, Vec<String>),
    ) -> Findings {
        if outcome.strays_stopped {
            limitations.push(
                "left processes running when it exited; they were stopped, so nothing a step starts outlives it".into(),
            );
        }
        if outcome.output_detached {
            limitations.push(
                "a process that left the step's process group still held its output; captured output may be incomplete".into(),
            );
        }
        let mut tolerated = false;
        let status = if outcome.timed_out {
            limitations.push(format!(
                "stopped at its {}s timeout budget; output is partial",
                budget.timeout_secs
            ));
            StepStatus::Timeout
        } else if outcome.interrupted {
            limitations.push("stopped because flot was asked to stop; output is partial".into());
            StepStatus::Failed
        } else if outcome.output_overflow {
            limitations.push(format!(
                "stopped after {} bytes on one output stream, its max_output_bytes budget; output is cut there",
                budget.max_output_bytes
            ));
            StepStatus::Failed
        } else if outcome.exit_code == Some(0) {
            StepStatus::Ok
        } else if step.allow_failure {
            tolerated = true;
            limitations.push(format!(
                "{} but the step is declared allow_failure, so the run is not failed by it",
                ending(outcome)
            ));
            StepStatus::Ok
        } else {
            if let Some(signal) = outcome.signal {
                limitations.push(format!("was killed by signal {signal}"));
            }
            StepStatus::Failed
        };
        let (metrics, metrics_notes) = reported;
        Findings {
            status,
            tolerated,
            claims: Vec::new(),
            limitations,
            metrics,
            metrics_notes,
            escaped: outcome.escaped.clone(),
        }
    }

    /// A success missing a declared output fails, unless the step is
    /// declared `allow_failure`.
    pub(super) fn missing_outputs(&mut self, step: &Step, unmatched: &[String]) {
        if !self.status.is_ok() || unmatched.is_empty() {
            return;
        }
        let missing = unmatched.join(", ");
        if step.allow_failure {
            self.tolerated = true;
            self.limitations.push(format!(
                "produced no file for declared output {missing}, but the step is declared allow_failure"
            ));
        } else {
            self.limitations
                .push(format!("produced no file for declared output {missing}"));
            self.status = StepStatus::Failed;
        }
    }

    /// A step that changed files outside its `may_change` fails, unless it
    /// is declared `allow_failure`.
    pub(super) fn strayed(&mut self, step: &Step, strayed: Option<&[String]>) {
        let Some(strayed) = strayed.filter(|strayed| !strayed.is_empty()) else {
            return;
        };
        let list = strayed.join(", ");
        if step.allow_failure {
            self.tolerated = self.tolerated || self.status.is_ok();
            self.limitations.push(format!(
                "changed files outside may_change ({list}), but the step is declared allow_failure"
            ));
        } else {
            self.limitations
                .push(format!("changed files outside may_change: {list}"));
            if self.status.is_ok() {
                self.status = StepStatus::Failed;
            }
        }
    }

    /// Count what a proof of the step reported spending as the step's.
    pub(super) fn spent(&mut self, metrics: Option<Metrics>, notes: Vec<String>) {
        self.metrics = match (self.metrics, metrics) {
            (Some(step), Some(proof)) => Some(step.plus(proof)),
            (step, proof) => step.or(proof),
        };
        self.metrics_notes.extend(notes);
    }

    /// A process that left its group may still have been at work while the
    /// proof ran, or after: whatever the proof said is not evidence.
    pub(super) fn escapes(&mut self) {
        if self.escaped.is_empty() {
            return;
        }
        let pids = |traced: bool| -> Option<String> {
            let mut pids: Vec<u32> = self
                .escaped
                .iter()
                .filter(|escapee| escapee.traced == traced)
                .map(|escapee| escapee.pid)
                .collect();
            pids.sort_unstable();
            pids.dedup();
            let named: Vec<String> = pids.iter().map(u32::to_string).collect();
            (!named.is_empty()).then(|| {
                format!(
                    "{} {}",
                    if named.len() == 1 { "pid" } else { "pids" },
                    named.join(", ")
                )
            })
        };
        if let Some(pids) = pids(true) {
            self.limitations.push(format!(
                "a process it started left its process group and was stopped ({pids})"
            ));
        }
        if let Some(pids) = pids(false) {
            self.limitations.push(format!(
                "a process that left its process group was stopped while this step ran ({pids}); nothing tells which step started it, so each step running then answers for it"
            ));
        }
        // A refutation stands: the step is not let off by what escaped.
        for claim in &mut self.claims {
            if claim.status == ClaimStatus::Verified {
                claim.status = ClaimStatus::Unverified;
                claim.reason = Some("a process left its process group while the step ran".into());
            }
        }
    }
}

/// How a command that did not exit zero ended, for a limitation.
fn ending(outcome: &exec::Outcome) -> String {
    match (outcome.exit_code, outcome.signal) {
        (Some(code), _) => format!("exited {code}"),
        (None, Some(signal)) => format!("was killed by signal {signal}"),
        (None, None) => "ended without an exit status".into(),
    }
}
