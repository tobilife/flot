//! The run manifest: the steps a run recorded with their receipts, attempts
//! and candidates, and how a step's candidates rank.

use serde::{Deserialize, Serialize};

use crate::core::digest;
use crate::core::metrics::Spend;

use super::{ClaimCounts, Receipt, StepStatus, Verdict};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStep {
    pub step_id: String,
    pub receipt_id: String,
    pub status: StepStatus,
    /// True when the receipt was reused from a previous run instead of re-run.
    pub cached: bool,
    pub claims: ClaimCounts,
    /// What this run observed of the step: output previews, timing and
    /// reported spend, stored as an object named by this digest. The receipt
    /// id covers the work, not one execution of it, so each run keeps its own
    /// observation, and a signature over the manifest covers it.
    pub observed_digest: String,
    /// Earlier attempts this run made at the step, oldest first, each of
    /// which did not hold. The step's receipt is the last attempt's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<RunAttempt>,
    /// The step's other candidates this run executed, when it names
    /// candidates. The step's receipt is the kept candidate's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<RunCandidate>,
    /// Signer keys of the imported run the reused receipt came from: whose
    /// word stands in for the work here. Empty when this host executed the
    /// step, or reused a receipt it executed itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vouched_by: Vec<String>,
}

/// An attempt at a step that did not hold, kept as evidence of what it took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunAttempt {
    pub receipt_id: String,
    pub observed_digest: String,
}

/// A candidate of a step that this run executed and did not keep: its last
/// receipt, that receipt's observation, and its earlier attempts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCandidate {
    pub name: String,
    pub receipt_id: String,
    pub observed_digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<RunAttempt>,
}

/// How a step's candidates are ranked, best first: one whose last receipt
/// held, and among those lower reported cost over all its attempts, an
/// unknown cost after every known one. Candidates that did not hold rank
/// equal. Equal ranks keep the earlier candidate.
pub fn candidate_rank(last: &Receipt, cost_micro_usd: Option<u64>) -> (bool, bool, u64) {
    if !last.is_reusable() {
        return (true, false, 0);
    }
    (false, cost_micro_usd.is_none(), cost_micro_usd.unwrap_or(0))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunManifest {
    pub run_id: String,
    pub flow: String,
    pub flow_digest: String,
    /// Fold of the ordered step receipt ids. Deterministic flows reproduce it.
    pub root_digest: String,
    pub verdict: Verdict,
    /// Set when the run selected only some of the flow's steps: its verdict
    /// speaks for those, not for the flow. Absent from a run of every step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<Partial>,
    pub steps: Vec<RunStep>,
    pub claims: ClaimCounts,
    pub limitations: Vec<String>,
    /// How many steps this run was allowed to overlap.
    pub max_parallel: usize,
    /// Tokens and cost reported by steps, split into spent and reused.
    pub spend: Spend,
    /// Run cost ceiling in micro-dollars, when one was set.
    pub max_cost_micro_usd: Option<u64>,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub flot_version: String,
    /// Edits and commands the guard refused after the run before this one
    /// executed here finished, up to the end of this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refusals: Vec<Refusal>,
}

/// How many of a flow's steps a run selected, when not all of them: the step
/// asked for and every step it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partial {
    pub selected: usize,
    /// The steps the flow has.
    pub of: usize,
}

/// An edit or a shell command the guard refused an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub at_ms: u64,
    /// The agent's tool, such as `Edit` or `Bash`.
    pub tool: String,
    /// What it would have touched, and why that is refused.
    pub reasons: Vec<String>,
}

impl RunManifest {
    pub fn root_of(step_receipts: &[(String, String)]) -> String {
        let members: Vec<String> = step_receipts
            .iter()
            .map(|(step_id, receipt_id)| format!("{step_id}@{receipt_id}"))
            .collect();
        digest::fold("flot.run", &members)
    }

    pub fn step(&self, step_id: &str) -> Option<&RunStep> {
        self.steps.iter().find(|step| step.step_id == step_id)
    }

    /// The id a run with this start time and root must carry. Run ids name
    /// files in a store, so one that is not exactly this is never accepted.
    pub fn id_for(started_at_ms: u64, root_digest: &str) -> String {
        format!("{:013}-{}", started_at_ms, digest::short(root_digest))
    }

    /// Every receipt this run names with the observation it records for it:
    /// each step's, and each earlier attempt's.
    pub fn evidence(&self) -> Vec<(&str, &str, &str)> {
        let mut named = Vec::new();
        for step in &self.steps {
            for candidate in &step.candidates {
                for attempt in &candidate.attempts {
                    named.push((
                        step.step_id.as_str(),
                        attempt.receipt_id.as_str(),
                        attempt.observed_digest.as_str(),
                    ));
                }
                named.push((
                    step.step_id.as_str(),
                    candidate.receipt_id.as_str(),
                    candidate.observed_digest.as_str(),
                ));
            }
            for attempt in &step.attempts {
                named.push((
                    step.step_id.as_str(),
                    attempt.receipt_id.as_str(),
                    attempt.observed_digest.as_str(),
                ));
            }
            named.push((
                step.step_id.as_str(),
                step.receipt_id.as_str(),
                step.observed_digest.as_str(),
            ));
        }
        named
    }
}

/// Two costs together; unknown when either is, since an execution that
/// reported no cost may have cost anything.
pub fn sum_costs(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    Some(a?.saturating_add(b?))
}
