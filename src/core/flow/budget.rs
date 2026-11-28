//! How long a step may run and how much output it may produce and carry
//! inline: what the step sets, else what the flow sets, else flot's
//! defaults.

use serde::{Deserialize, Serialize};

use super::{Flow, Step};

pub const DEFAULT_TIMEOUT_SECS: u64 = 900;
pub const DEFAULT_MAX_INLINE_BYTES: usize = 4096;
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Admission limit: wall-clock ceiling for one step. Exceeding it stops the
    /// step and is reported as `timeout`, never as success.
    pub timeout_secs: Option<u64>,
    /// Delivery budget: how many output bytes travel inline. The complete
    /// output always stays retrievable from the content store.
    pub max_inline_bytes: Option<usize>,
    /// Admission limit: how many bytes one output stream may produce. A step
    /// past it is stopped and failed; flot holds output in memory and in the
    /// store, so a runaway stream must not be able to exhaust either.
    pub max_output_bytes: Option<usize>,
}

impl Budget {
    fn resolve(&self, parent: &Budget) -> ResolvedBudget {
        ResolvedBudget {
            timeout_secs: self
                .timeout_secs
                .or(parent.timeout_secs)
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
            max_inline_bytes: self
                .max_inline_bytes
                .or(parent.max_inline_bytes)
                .unwrap_or(DEFAULT_MAX_INLINE_BYTES),
            max_output_bytes: self
                .max_output_bytes
                .or(parent.max_output_bytes)
                .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedBudget {
    pub timeout_secs: u64,
    pub max_inline_bytes: usize,
    pub max_output_bytes: usize,
}

impl Flow {
    pub fn resolved_budget(&self, step: &Step) -> ResolvedBudget {
        step.budget.resolve(&self.meta.budget)
    }
}
