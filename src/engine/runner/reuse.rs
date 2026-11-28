//! Receipts against the workspace: whether a recorded receipt may stand in
//! for running a step, writing back the outputs it recorded, which files
//! earlier receipts still answer for, and outputs two steps both produce.

mod conflicts;
mod in_place;
mod restore;

use anyhow::{Context, Result};

use crate::core::digest;
use crate::core::flow::{is_contained, Step};
use crate::engine::keying;
use crate::store::Recalled;

use super::Runner;

impl Runner<'_> {
    /// A reusable receipt for this key, with why one was passed over.
    ///
    /// Reuse writes the receipt's outputs back, so a receipt naming a file this
    /// flow does not declare for the step is never reused. Neither is one whose
    /// outputs were changed since, or that a file now matching an output
    /// pattern is missing from: its claims were proven against the recorded
    /// content, and both overwriting the change and keeping it under that
    /// receipt would leave a verdict about files that are not there. A missing
    /// output is fine; reuse restores it. The other files the step changed
    /// must be as it left them too, for the same reason; those are not
    /// restored, since a change there is someone else's work.
    ///
    /// An imported receipt comes with its signers, so the run can record
    /// whose word stands in for the work.
    pub(super) fn recall(
        &self,
        step: &Step,
        step_key: &str,
        notes: &mut Vec<String>,
    ) -> Result<Option<Recalled>> {
        let Some(recalled) = self.store.recall(step_key)? else {
            return Ok(None);
        };
        // An imported receipt stands in for local work only while a signer
        // of its bundle is still trusted: revoking a signer revokes this too.
        if !recalled.vouched_by.is_empty() {
            let trusted = self.store.trusted_keys()?;
            if !recalled
                .vouched_by
                .iter()
                .any(|signer| trusted.contains_key(signer))
            {
                notes.push(
                    "cached receipt came from a signer this store no longer trusts; not reused"
                        .into(),
                );
                return Ok(None);
            }
        }
        let known = &recalled.receipt;
        // The key covers the claims; this guards the reuse decision itself.
        let claimed: Vec<&str> = known
            .identity
            .claims
            .iter()
            .map(|claim| claim.text.as_str())
            .collect();
        if claimed != step.claims.iter().map(String::as_str).collect::<Vec<_>>() {
            notes.push(
                "cached receipt settled other claims than this step makes; not reused".into(),
            );
            return Ok(None);
        }
        // Whether the proof fails before the change depends on the revision
        // too, which the key does not cover.
        if step.fails_before {
            let checked_at = known
                .identity
                .claims
                .iter()
                .find_map(|claim| claim.before.as_ref())
                .map(|before| before.commit.as_str());
            if checked_at != self.baseline.map(|baseline| baseline.commit.as_str()) {
                notes.push(
                    "cached receipt was not checked against this revision before the change; the step runs again"
                        .into(),
                );
                return Ok(None);
            }
        }
        // So does what the held files are at the revision.
        if !step.held.is_empty() {
            let held_at = known
                .identity
                .claims
                .iter()
                .find_map(|claim| claim.held.as_ref())
                .map(|held| held.base.clone());
            if held_at != self.baseline.map(|baseline| baseline.held_base(&step.held)) {
                notes.push(
                    "cached receipt was not proven with the held files as this revision has them; the step runs again"
                        .into(),
                );
                return Ok(None);
            }
        }
        // And which mutants were tested: the run may mutate the changes
        // since the revision.
        if step.mutants.is_some() {
            let tested_at = known
                .identity
                .claims
                .iter()
                .find_map(|claim| claim.mutants.as_ref())
                .map(|mutants| mutants.commit.as_deref());
            if tested_at != Some(self.baseline.map(|baseline| baseline.commit.as_str())) {
                notes.push(
                    "cached receipt was not tested against mutants with this revision; the step runs again"
                        .into(),
                );
                return Ok(None);
            }
        }
        for output in &known.identity.outputs {
            if !step.declares_output(&output.path) {
                notes.push(format!(
                    "cached receipt names {}, which this step does not declare as an output; not reused",
                    output.path
                ));
                return Ok(None);
            }
            if !keying::within_root(&self.root, &output.path)? {
                notes.push(format!(
                    "output {} is reached through a symbolic link; the cached receipt is not reused",
                    output.path
                ));
                return Ok(None);
            }
            let path = self.root.join(&output.path);
            match std::fs::read(&path) {
                Ok(bytes)
                    if digest::bytes(&bytes) != output.digest
                        || keying::is_executable(&path) != output.executable =>
                {
                    notes.push(format!(
                        "output {} changed since its receipt; the step runs again",
                        output.path
                    ));
                    return Ok(None);
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("cannot read output {}", output.path))
                }
            }
        }
        if !step.outputs.is_empty() {
            let now =
                keying::scan_outputs(&self.root, &step.outputs, |bytes| Ok(digest::bytes(bytes)))?;
            if let Some(file) = now.files.iter().find(|file| {
                !known
                    .identity
                    .outputs
                    .iter()
                    .any(|output| output.path == file.path)
            }) {
                notes.push(format!(
                    "{} matches an output pattern but is not in its receipt; the step runs again",
                    file.path
                ));
                return Ok(None);
            }
        }
        for (path, left) in &known.identity.changed {
            if !is_contained(path) {
                notes.push(format!(
                    "cached receipt names {path}, which is outside the flow root; not reused"
                ));
                return Ok(None);
            }
            if keying::content_of(&self.root, path)? != *left {
                notes.push(match left {
                    Some(_) => format!("{path} changed since its receipt; the step runs again"),
                    None => format!(
                        "{path}, which the step deleted, is there again; the step runs again"
                    ),
                });
                return Ok(None);
            }
        }
        Ok(Some(recalled))
    }

    /// A cached receipt with its missing outputs put back, or `None` with the
    /// reason noted when there is none to reuse or it cannot be restored. A
    /// failed restore is a miss, not an error: running the step again is
    /// always a valid way to get its receipt.
    pub(super) fn reuse(
        &self,
        step: &Step,
        step_key: &str,
        limitations: &mut Vec<String>,
    ) -> Option<(Recalled, Vec<String>)> {
        let known = match self.recall(step, step_key, limitations) {
            Ok(known) => known?,
            Err(error) => {
                limitations.push(format!(
                    "cached receipt not reused: {error:#}; the step runs again"
                ));
                return None;
            }
        };
        match self.restore_outputs(&known.receipt) {
            Ok(notes) => Some((known, notes)),
            Err(error) => {
                limitations.push(format!(
                    "cached receipt not reused: {error:#}; the step runs again"
                ));
                None
            }
        }
    }
}
