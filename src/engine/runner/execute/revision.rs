//! The proof run on a copy of the work against the baseline revision:
//! before the change, and with the held files as the revision has them.

use anyhow::{Context, Result};

use crate::core::flow::{ResolvedBudget, Step};
use crate::core::metrics::Metrics;
use crate::engine::claims::{Hold, Proof, Red};
use crate::engine::exec;
use crate::engine::keying;
use crate::engine::runner::{RunOptions, Runner};

use super::verify::Place;

impl Runner<'_> {
    /// What `step`'s proof does before the change: it runs on a copy of the
    /// work, where it must pass, then again there with every change since
    /// the baseline undone. Both runs count as the step's spend.
    pub(super) fn red(
        &self,
        step: &Step,
        budget: ResolvedBudget,
        options: &RunOptions,
        escaped: &mut Vec<exec::Escaped>,
    ) -> (Red, Option<Metrics>, Vec<String>) {
        let Some(baseline) = self.baseline else {
            return (Red::NoRevision, None, Vec::new());
        };
        let command = step
            .verify
            .as_deref()
            .expect("a step with fails_before has a verify command");
        let mut spent: Option<Metrics> = None;
        let mut notes = Vec::new();
        let mut prove = |place: Place| -> Result<Proof> {
            let (proof, metrics, proof_notes) =
                self.prove(step, command, place, budget, options, escaped)?;
            spent = match (spent.take(), metrics) {
                (Some(spent), Some(more)) => Some(spent.plus(more)),
                (spent, more) => spent.or(more),
            };
            notes.extend(
                proof_notes
                    .into_iter()
                    .map(|note| format!("before the change, {note}")),
            );
            Ok(proof)
        };
        let red = (|| -> Result<Red> {
            let builds = self.store.builds(&|| options.stop.is_set())?;
            let copy = self.copy(step, &builds)?;
            let green = prove(Place::Copy(&copy))?;
            if green.timed_out || green.exit_code != Some(0) {
                return Ok(Red::CopyFails {
                    exit_code: green.exit_code,
                    timed_out: green.timed_out,
                });
            }
            baseline
                .undo(copy.dir())
                .context("cannot undo the changes in the copy")?;
            Ok(Red::Ran {
                baseline: baseline.label(),
                commit: baseline.commit.clone(),
                proof: prove(Place::Copy(&copy))?,
            })
        })()
        .unwrap_or_else(|error| Red::Unchecked(format!("{error:#}")));
        (red, spent, notes)
    }

    /// What `step`'s proof does with its `held` files as the revision has
    /// them: nothing to run when they are here as they are there, else it
    /// runs on a copy of the work with them put back. That run counts as the
    /// step's spend.
    pub(super) fn held(
        &self,
        step: &Step,
        budget: ResolvedBudget,
        options: &RunOptions,
        escaped: &mut Vec<exec::Escaped>,
    ) -> (Hold, Option<Metrics>, Vec<String>) {
        let Some(baseline) = self.baseline else {
            return (Hold::NoRevision, None, Vec::new());
        };
        let command = step
            .verify
            .as_deref()
            .expect("a step with held files has a verify command");
        let base = baseline.held_base(&step.held);
        let here = keying::held_files(&self.root, &step.held, &self.digests);
        let files = match baseline.held_changes(&step.held, &here) {
            Ok(files) => files,
            Err(error) => return (Hold::Unchecked(format!("{error:#}")), None, Vec::new()),
        };
        if files.is_empty() {
            return (Hold::Same { base }, None, Vec::new());
        }
        let ran = (|| -> Result<(Proof, Option<Metrics>, Vec<String>)> {
            let builds = self.store.builds(&|| options.stop.is_set())?;
            let copy = self.copy(step, &builds)?;
            baseline
                .hold(copy.dir(), &files)
                .context("cannot put the held files back in the copy")?;
            self.prove(step, command, Place::Copy(&copy), budget, options, escaped)
        })();
        match ran {
            Ok((proof, metrics, notes)) => (
                Hold::Ran {
                    baseline: baseline.label(),
                    base,
                    files,
                    proof,
                },
                metrics,
                notes
                    .into_iter()
                    .map(|note| format!("with the held files as the revision has them, {note}"))
                    .collect(),
            ),
            Err(error) => (Hold::Unchecked(format!("{error:#}")), None, Vec::new()),
        }
    }
}
