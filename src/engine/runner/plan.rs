//! Planning a run: what each selected step would do, executed or reused,
//! and the evidence gaps the flow already has, without running anything.

use std::collections::{BTreeSet, HashMap};

use anyhow::Result;
use serde::Serialize;

use crate::core::receipt::FileDigest;
use crate::engine::claims::ProofOutcome;
use crate::engine::keying;

use super::{RunOptions, Runner};

/// Predicted execution plan without side effects.
#[derive(Debug, Clone, Serialize)]
pub struct PlanEntry {
    pub step_id: String,
    pub step_key: Option<String>,
    pub cached: bool,
    pub needs: Vec<String>,
    pub claims: usize,
    pub verified_by: Option<String>,
    pub inputs: usize,
    pub outputs: Vec<String>,
    pub limitations: Vec<String>,
    /// The `always` steps this step's key, and so its reuse, is predicted on:
    /// the plan takes each to come out as it did last time, which only
    /// running it can tell.
    pub assumes: Vec<String>,
}

impl Runner<'_> {
    /// Plan the run: what would execute, what would be reused, what evidence
    /// gaps the flow already has. Reads files, runs nothing.
    pub fn plan(&self, options: &RunOptions) -> Result<Vec<PlanEntry>> {
        let order = self.selection(options)?;
        let mut receipts: HashMap<String, String> = HashMap::new();
        // For each step whose receipt is predicted, the `always` steps that
        // prediction rests on.
        let mut assumed: HashMap<String, BTreeSet<String>> = HashMap::new();
        // Outputs that reusing a predicted-cached step will put back. A
        // dependent's inputs are digested as they will be once that happened.
        let mut pending: Vec<FileDigest> = Vec::new();
        let mut plan = Vec::new();
        for step_id in order {
            let step = self.flow.step(&step_id).expect("validated step");
            let facts = keying::facts_with(&self.root, step, &pending, &self.digests);
            let mut limitations = facts.limitations.clone();
            let drift = self.drift(step, &facts.verify_inputs);
            if let Some(ProofOutcome::Unanchored {
                baseline, reasons, ..
            }) = &drift
            {
                limitations.push(format!(
                    "the proof differs from {baseline} ({}); its claims would stay unverified",
                    reasons.join(", ")
                ));
            }
            if step.always {
                limitations.push(
                    "runs on every run (always); the steps that need it are reused only when its receipt comes out the same, which running it decides"
                        .into(),
                );
            }
            let force = options.force || drift.is_some() || step.always;
            // A dependency with no known receipt makes the downstream key
            // unpredictable until the dependency actually runs.
            let (step_key, reusable) = match keying::dep_refs(step, &receipts) {
                Some(deps) if !step.candidates.is_empty() => {
                    let reusable = if force {
                        None
                    } else {
                        self.best_cached_candidate(step, &deps, &pending, &mut limitations)
                            .map(|recalled| recalled.receipt)
                    };
                    (
                        reusable
                            .as_ref()
                            .map(|receipt| receipt.identity.step_key.clone()),
                        reusable,
                    )
                }
                Some(deps) => {
                    let key = self.step_key(step, &facts, &deps);
                    let reusable = if force {
                        None
                    } else {
                        // As in `run`: a receipt that cannot be checked is a
                        // miss, and the step would run.
                        self.recall(step, &key, &mut limitations)
                            .unwrap_or_else(|error| {
                                limitations.push(format!(
                                    "cached receipt not reused: {error:#}; the step runs again"
                                ));
                                None
                            })
                            .map(|recalled| recalled.receipt)
                    };
                    (Some(key), reusable)
                }
                None => (None, None),
            };
            let assumes: BTreeSet<String> = step
                .needs
                .iter()
                .filter_map(|need| assumed.get(need))
                .flatten()
                .cloned()
                .collect();
            // An `always` step runs, but what it recorded last time for this
            // key is what it comes out as when nothing it probes changed:
            // what follows it is predicted on that.
            let reusable = match (reusable, &step_key) {
                (None, Some(key)) if step.always && !options.force => {
                    self.store.recall(key)?.map(|recalled| recalled.receipt)
                }
                (reusable, _) => reusable,
            };
            if reusable.is_some() {
                let mut passed_on = assumes.clone();
                if step.always {
                    passed_on.insert(step_id.clone());
                }
                assumed.insert(step_id.clone(), passed_on);
            }
            if step.claims.is_empty() {
                limitations.push("step declares no claims; its success proves nothing".into());
            } else if step.verify.is_none() {
                limitations.push("step declares claims without a verify command".into());
            }
            let cached = reusable.is_some() && !step.always;
            if let Some(receipt) = reusable {
                pending.extend(
                    receipt
                        .identity
                        .outputs
                        .iter()
                        .filter(|output| !self.root.join(&output.path).exists())
                        .cloned(),
                );
                receipts.insert(step_id.clone(), receipt.receipt_id);
            }
            plan.push(PlanEntry {
                step_id,
                step_key,
                cached,
                needs: step.needs.clone(),
                claims: step.claims.len(),
                verified_by: step.verify.clone(),
                inputs: facts.inputs.len() + facts.verify_inputs.len(),
                outputs: step.outputs.clone(),
                limitations,
                assumes: assumes.into_iter().collect(),
            });
        }
        Ok(plan)
    }
}
