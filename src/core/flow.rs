//! Flow definition: parsing, validation and dependency ordering.
//!
//! A flow is declarative data. It names steps, what each step runs, what files
//! it reads, what it claims, and the command that proves the claim. Nothing
//! here executes anything; `exec` and `run` own that.

mod budget;
mod order;
mod parse;
mod step;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub use budget::{
    Budget, ResolvedBudget, DEFAULT_MAX_INLINE_BYTES, DEFAULT_MAX_OUTPUT_BYTES,
    DEFAULT_TIMEOUT_SECS,
};
pub use step::{is_contained, is_valid_step_id, Mutants, Sandbox, Step, CANDIDATE_ENV};

/// Schema version participates in every step key: a change in how flot derives
/// identity must invalidate cached receipts rather than silently reuse them.
pub const SCHEMA_VERSION: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowMeta {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub budget: Budget,
    /// How many steps may run at once. Concurrency is opt-in because flot can
    /// digest what a step reads but cannot prove what it writes, so only the
    /// flow author knows whether two steps are safe to overlap.
    #[serde(default)]
    pub max_parallel: Option<usize>,
    /// Run cost ceiling in dollars, checked before each step executes against
    /// what executed steps reported through `$FLOT_METRICS`.
    #[serde(default)]
    pub max_cost_usd: Option<f64>,
    /// Git revision every proof must match, such as `origin/main`. The
    /// committed flow's setting wins over the working copy's, so removing it
    /// takes a commit.
    #[serde(default)]
    pub proofs_from: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Flow {
    pub meta: FlowMeta,
    pub steps: Vec<Step>,
    index: HashMap<String, usize>,
}

impl Flow {
    pub fn step(&self, id: &str) -> Option<&Step> {
        self.index.get(id).map(|position| &self.steps[*position])
    }

    /// Identity of the flow definition itself, independent of declaration order.
    pub fn digest(&self) -> String {
        let mut steps: Vec<&Step> = self.steps.iter().collect();
        steps.sort_by(|a, b| a.id.cmp(&b.id));
        crate::core::digest::canonical(&serde_json::json!({
            "schema": SCHEMA_VERSION,
            "name": self.meta.name,
            "steps": steps,
        }))
    }

    /// Steps that declare claims without a verify command. These are reported,
    /// not refused: an unproven claim is a known limitation of the run.
    pub fn unproven_claim_steps(&self) -> Vec<&Step> {
        self.steps
            .iter()
            .filter(|step| !step.claims.is_empty() && step.verify.is_none())
            .collect()
    }
}

#[cfg(test)]
mod tests;
