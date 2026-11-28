//! What a step leaves behind: the outputs kept in the store, and the
//! changed files its receipt binds.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::core::digest;
use crate::core::flow::{ResolvedBudget, Step};
use crate::core::preview::stream_preview;
use crate::core::receipt::FileDigest;
use crate::engine::exec;
use crate::engine::keying::{self, Facts};
use crate::engine::runner::{Keyed, Runner};

use super::{Findings, Produced};

impl Runner<'_> {
    /// Whether a receipt binds what its step changed. A candidate's changes
    /// other than its outputs stay in its copy: there is nothing to bind.
    pub(super) fn binds(&self) -> bool {
        self.candidate.is_none()
    }

    /// Keep whatever the step produced, even when it failed: partial output
    /// is evidence of how it failed. A success that is missing a declared
    /// output did not do what the flow says it does.
    pub(super) fn keep_outputs(
        &self,
        step: &Step,
        budget: ResolvedBudget,
        outcome: &exec::Outcome,
        found: &mut Findings,
    ) -> Result<Produced> {
        let stdout_digest = self.store.put_bytes(&outcome.stdout)?;
        let stderr_digest = self.store.put_bytes(&outcome.stderr)?;
        let scan = match keying::scan_outputs(&self.root, &step.outputs, |bytes| {
            self.store.put_bytes(bytes)
        }) {
            Ok(scan) => scan,
            Err(error) => {
                found
                    .limitations
                    .push(format!("outputs could not be recorded: {error:#}"));
                keying::Scan {
                    unmatched: step.outputs.clone(),
                    ..keying::Scan::default()
                }
            }
        };
        found.limitations.extend(scan.problems);
        found.missing_outputs(step, &scan.unmatched);
        let (stdout_inline, stdout_truncated) =
            stream_preview(&outcome.stdout, budget.max_inline_bytes);
        let (stderr_inline, stderr_truncated) =
            stream_preview(&outcome.stderr, budget.max_inline_bytes);
        Ok(Produced {
            stdout_digest,
            stderr_digest,
            stdout_inline,
            stdout_truncated,
            stderr_inline,
            stderr_truncated,
            files: scan.files,
        })
    }

    /// Re-read as `after` what the step declared it reads, and bind to its
    /// receipt the files it answers for besides its outputs. A step that
    /// edits its own inputs leaves a receipt describing content that no
    /// longer exists; one that edits its proof cannot have that proof vouch
    /// for it.
    pub(super) fn bind_changes(
        &self,
        keyed: &Keyed<'_>,
        after: &Facts,
        changed: &mut BTreeMap<String, Option<String>>,
        limitations: &mut Vec<String>,
    ) {
        let (step, facts) = (keyed.step, &keyed.facts);
        let edited_inputs = keying::changed_paths(&facts.inputs, &after.inputs);
        if !edited_inputs.is_empty() {
            limitations.push(format!(
                "the step changed its own declared inputs ({}); this receipt describes them as they were before it ran",
                edited_inputs.join(", ")
            ));
        }
        if !self.binds() {
            return;
        }
        // Declared files the scan does not reach, such as an input git
        // ignores, are bound too.
        for (path, content) in keying::changes(&facts.inputs, &after.inputs)
            .into_iter()
            .chain(keying::changes(&facts.verify_inputs, &after.verify_inputs))
        {
            changed.entry(path).or_insert(content);
        }
        for (path, content) in self.still_in_place(step, &keyed.step_key) {
            changed.entry(path).or_insert(content);
        }
        // Outputs are recorded, restored and checked as outputs.
        changed.retain(|path, _| !step.declares_output(path));
    }

    /// Recorded outputs that now differ from what was recorded, or that are
    /// gone, and files matching an output pattern that appeared since.
    pub(super) fn changed_outputs(&self, step: &Step, recorded: &[FileDigest]) -> Vec<String> {
        let now = match keying::scan_globs(
            &self.root,
            &step.outputs,
            "output",
            keying::Reach::Everything,
            |bytes| Ok::<_, std::convert::Infallible>(digest::bytes(bytes)),
        ) {
            Ok(scan) => scan.files,
            Err(never) => match never {},
        };
        keying::changed_paths(recorded, &now)
    }
}
