//! Outputs two steps both produce where neither needs the other, so which
//! content survives depends on scheduling.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;

use crate::engine::runner::{Runner, Settled};

impl Runner<'_> {
    /// Files produced by two steps where neither needs the other. Which content
    /// survives then depends on scheduling, so one receipt describes a file
    /// that is no longer there.
    pub(in crate::engine::runner) fn output_conflicts(
        &self,
        order: &[String],
        settled: &HashMap<String, Settled>,
    ) -> Result<Vec<String>> {
        let mut producers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for step_id in order {
            for output in &settled[step_id].receipt.identity.outputs {
                producers
                    .entry(output.path.as_str())
                    .or_default()
                    .push(step_id.as_str());
            }
        }
        let mut conflicts = Vec::new();
        for (path, steps) in producers {
            let mut rest = steps.as_slice();
            while let Some((first, later)) = rest.split_first() {
                rest = later;
                for second in later {
                    let ordered = self.flow.closure(second)?.iter().any(|id| id == first)
                        || self.flow.closure(first)?.iter().any(|id| id == second);
                    if !ordered {
                        conflicts.push(format!(
                            "steps {first} and {second} both produce {path} and neither needs the other; which content survives depends on scheduling"
                        ));
                    }
                }
            }
        }
        Ok(conflicts)
    }
}
