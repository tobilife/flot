//! Claim counts, and the verdict a run's steps and claims come to.

use serde::{Deserialize, Serialize};

use super::RunStep;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimCounts {
    pub verified: usize,
    pub unverified: usize,
    pub refuted: usize,
}

impl ClaimCounts {
    pub fn total(&self) -> usize {
        self.verified + self.unverified + self.refuted
    }

    pub fn add(&mut self, other: ClaimCounts) {
        self.verified += other.verified;
        self.unverified += other.unverified;
        self.refuted += other.refuted;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every step reached `Ok` and every declared claim is verified.
    Passed,
    /// Work completed but at least one claim has no proof behind it.
    Unverified,
    /// A step failed, timed out, or a claim was refuted.
    Failed,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Passed => "passed",
            Verdict::Unverified => "unverified",
            Verdict::Failed => "failed",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Verdict::Passed => 0,
            Verdict::Unverified => 2,
            Verdict::Failed => 1,
        }
    }
}

/// The verdict rule, in one place: failures dominate, then refuted claims, then
/// unproven claims. Success requires proof, not absence of failure.
pub fn verdict_for(steps: &[RunStep], claims: ClaimCounts) -> Verdict {
    if steps.iter().any(|step| !step.status.is_ok()) || claims.refuted > 0 {
        return Verdict::Failed;
    }
    if claims.unverified > 0 {
        return Verdict::Unverified;
    }
    Verdict::Passed
}
