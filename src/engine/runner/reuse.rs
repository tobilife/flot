//! Receipts against the workspace: whether a recorded receipt may stand in
//! for running a step, writing back the outputs it recorded, which files
//! earlier receipts still answer for, and outputs two steps both produce.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};

use crate::core::digest;
use crate::core::flow::{is_contained, Step};
use crate::core::receipt::Receipt;
use crate::engine::keying;
use crate::store::Recalled;

use super::{Runner, Settled};

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

    /// Write the kept candidate's outputs into the workspace, replacing what
    /// is there: they are what its claims were proven against.
    pub(super) fn write_back(&self, receipt: &Receipt) -> Vec<String> {
        let mut notes = Vec::new();
        for output in &receipt.identity.outputs {
            let written = (|| -> Result<()> {
                if !keying::within_root(&self.root, &output.path)? {
                    anyhow::bail!("it is reached through a symbolic link");
                }
                let path = self.root.join(&output.path);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, self.store.read_bytes(&output.digest)?)?;
                #[cfg(unix)]
                if output.executable {
                    use std::os::unix::fs::PermissionsExt;
                    let mut permissions = std::fs::metadata(&path)?.permissions();
                    permissions.set_mode(permissions.mode() | 0o111);
                    std::fs::set_permissions(&path, permissions)?;
                }
                Ok(())
            })();
            if let Err(error) = written {
                notes.push(format!(
                    "output {} of the kept candidate could not be written back: {error:#}",
                    output.path
                ));
            }
        }
        notes
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

    /// Put back the outputs of a reused receipt that are missing, so the files
    /// on disk are the ones its claims were proven against. `recall` already
    /// refused any receipt whose outputs exist with other content.
    pub(super) fn restore_outputs(&self, receipt: &Receipt) -> Result<Vec<String>> {
        let mut notes = Vec::new();
        for output in &receipt.identity.outputs {
            let path = self.root.join(&output.path);
            if path.exists() {
                continue;
            }
            // Checked again here: nothing may be written through a link.
            if !keying::within_root(&self.root, &output.path)? {
                anyhow::bail!(
                    "refusing to restore {}: it is reached through a symbolic link",
                    output.path
                );
            }
            let bytes = self.store.read_bytes(&output.digest)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
            std::fs::write(&path, bytes)
                .with_context(|| format!("cannot restore output {}", output.path))?;
            #[cfg(unix)]
            if output.executable {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&path)?.permissions();
                permissions.set_mode(permissions.mode() | 0o111);
                std::fs::set_permissions(&path, permissions)
                    .with_context(|| format!("cannot make {} executable", output.path))?;
            }
            notes.push(format!(
                "output {} was missing and was restored from its receipt",
                output.path
            ));
        }
        Ok(notes)
    }

    /// Files produced by two steps where neither needs the other. Which content
    /// survives then depends on scheduling, so one receipt describes a file
    /// that is no longer there.
    pub(super) fn output_conflicts(
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
            for (position, first) in steps.iter().enumerate() {
                for second in &steps[position + 1..] {
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

    /// The files earlier receipts of `step` answer for that are still as
    /// those receipts say the step left them: the receipt this key would
    /// reuse, and the step's receipt in the newest run here. A run that finds
    /// that work already done (a file it writes already holds what it writes,
    /// one it deletes is already gone) changes nothing there, and must answer
    /// for it all the same. A file someone else has changed since is no
    /// longer the step's doing, and is left out.
    pub(super) fn still_in_place(
        &self,
        step: &Step,
        step_key: &str,
    ) -> BTreeMap<String, Option<String>> {
        let latest = self.latest.get_or_init(|| {
            self.store
                .resolve_run("latest")
                .ok()
                .filter(|run| run.flow == self.flow.meta.name)
                .map(|run| {
                    run.steps
                        .into_iter()
                        .map(|entry| (entry.step_id, entry.receipt_id))
                        .collect()
                })
                .unwrap_or_default()
        });
        let earlier = self
            .store
            .recall(step_key)
            .ok()
            .flatten()
            .map(|recalled| recalled.receipt)
            .into_iter()
            .chain(
                latest
                    .get(&step.id)
                    .and_then(|receipt_id| self.store.load_receipt(receipt_id).ok()),
            );
        let mut kept = BTreeMap::new();
        for receipt in earlier {
            if receipt.identity.flow != self.flow.meta.name || receipt.identity.step_id != step.id {
                continue;
            }
            for (path, left) in receipt.identity.changed {
                if is_contained(&path)
                    && keying::content_of(&self.root, &path).is_ok_and(|now| now == left)
                {
                    kept.insert(path, left);
                }
            }
        }
        kept
    }
}
