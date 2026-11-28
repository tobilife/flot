//! What one step of a run printed, by candidate, attempt and stream, and the
//! stored objects that hold it.

use anyhow::{bail, Result};
use serde::Serialize;

use crate::core::preview::inline_preview;
use crate::service::Workspace;

/// Which execution of a step [`Workspace::step_output`] reads.
#[derive(Debug, Clone, Default)]
pub struct OutputOf {
    /// A candidate by name; by default, the one the run kept.
    pub candidate: Option<String>,
    /// An attempt, numbered from 1; by default, the last.
    pub attempt: Option<usize>,
}

/// Which output of a step [`Workspace::step_output`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
    /// What the step's proof printed, stdout and stderr together.
    Proof,
    /// What the step's proof printed with its `held` files as the revision
    /// has them.
    Held,
    /// What the step's mutation tester printed.
    Mutants,
}

/// A stored object under a delivery budget. The complete object stays
/// retrievable; `recovery` says how when the view is cut.
#[derive(Debug, Clone, Serialize)]
pub struct ObjectView {
    pub digest: String,
    pub bytes: usize,
    pub truncated: bool,
    pub text: String,
    pub recovery: Option<String>,
}

impl Workspace {
    /// The digest of the complete output one step of a run printed, or its
    /// proof printed, as `stream` says. Read it with [`Workspace::object`] or
    /// [`Workspace::read_object`]. By default that is the receipt the run
    /// kept; `which` can name another candidate the run executed, an earlier
    /// attempt, or both. Without a step it names the run's steps.
    pub fn step_output(
        &self,
        run: &str,
        step: Option<&str>,
        which: &OutputOf,
        stream: Stream,
    ) -> Result<String> {
        let manifest = self.store()?.resolve_run(run)?;
        let steps = || {
            manifest
                .steps
                .iter()
                .map(|entry| entry.step_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let Some(step) = step else {
            bail!(
                "name the step whose output to print; the steps of run {} are {}",
                manifest.run_id,
                steps()
            );
        };
        let Some(entry) = manifest.steps.iter().find(|entry| entry.step_id == step) else {
            bail!(
                "run {} has no step `{step}`; its steps are {}",
                manifest.run_id,
                steps()
            );
        };
        // The receipts one candidate's attempts left, oldest first.
        let mut tries: Vec<&str> = Vec::new();
        let kept = self.receipt(&entry.receipt_id)?;
        let other = match which.candidate.as_deref() {
            Some(name) if kept.identity.candidate.as_deref() != Some(name) => {
                let Some(other) = entry.candidates.iter().find(|c| c.name == name) else {
                    let ran: Vec<&str> = kept
                        .identity
                        .candidate
                        .as_deref()
                        .into_iter()
                        .chain(entry.candidates.iter().map(|c| c.name.as_str()))
                        .collect();
                    if ran.is_empty() {
                        bail!("step `{step}` of run {} has no candidates", manifest.run_id);
                    }
                    bail!(
                        "step `{step}` of run {} ran no candidate `{name}`; it ran {}",
                        manifest.run_id,
                        ran.join(", ")
                    );
                };
                Some(other)
            }
            _ => None,
        };
        match other {
            Some(other) => {
                tries.extend(other.attempts.iter().map(|a| a.receipt_id.as_str()));
                tries.push(&other.receipt_id);
            }
            None => {
                tries.extend(entry.attempts.iter().map(|a| a.receipt_id.as_str()));
                tries.push(&entry.receipt_id);
            }
        }
        let receipt_id = match which.attempt {
            None => tries[tries.len() - 1],
            Some(number) if (1..=tries.len()).contains(&number) => tries[number - 1],
            Some(number) => bail!(
                "attempt {number} is not recorded: step `{step}`{} of run {} took {} attempt(s)",
                which
                    .candidate
                    .as_deref()
                    .map(|name| format!(" candidate {name}"))
                    .unwrap_or_default(),
                manifest.run_id,
                tries.len()
            ),
        };
        let identity = if receipt_id == kept.receipt_id {
            kept.identity
        } else {
            self.receipt(receipt_id)?.identity
        };
        Ok(match stream {
            Stream::Stdout => identity.stdout_digest,
            Stream::Stderr => identity.stderr_digest,
            // One proof command proves every claim of a step, so any claim
            // that ran it holds its output.
            Stream::Proof => match identity
                .claims
                .iter()
                .find_map(|claim| claim.verify_output_digest.clone())
            {
                Some(digest) => digest,
                None => bail!(
                    "step `{step}` of run {} ran no proof{}",
                    manifest.run_id,
                    identity
                        .claims
                        .iter()
                        .find_map(|claim| claim.reason.as_deref())
                        .map(|reason| format!(": {reason}"))
                        .unwrap_or_else(|| ": it makes no claims".into())
                ),
            },
            Stream::Held => match identity
                .claims
                .iter()
                .find_map(|claim| claim.held.as_ref()?.output_digest.clone())
            {
                Some(digest) => digest,
                None => bail!(
                    "step `{step}` of run {} ran no proof with held files: {}",
                    manifest.run_id,
                    if identity.claims.iter().any(|claim| claim.held.is_some()) {
                        "they are here as the revision has them, so its proof already ran with them"
                    } else {
                        "it holds no files to a revision, or its proof did not pass first"
                    }
                ),
            },
            Stream::Mutants => match identity
                .claims
                .iter()
                .find_map(|claim| claim.mutants.as_ref())
            {
                Some(mutants) => mutants.output_digest.clone(),
                None => bail!(
                    "step `{step}` of run {} tested no mutants: it declares none, or its proof did not pass first",
                    manifest.run_id
                ),
            },
        })
    }

    /// The complete bytes of a stored object, authenticated against its digest.
    pub fn object(&self, digest: &str) -> Result<Vec<u8>> {
        self.store()?.read_bytes(digest)
    }

    pub fn read_object(&self, digest: &str, max_bytes: usize) -> Result<ObjectView> {
        let bytes = self.object(digest)?;
        let (text, truncated) = inline_preview(&bytes, max_bytes);
        Ok(ObjectView {
            digest: digest.to_string(),
            bytes: bytes.len(),
            truncated,
            text,
            recovery: truncated
                .then(|| format!("raise max_bytes above {} to read more", bytes.len())),
        })
    }
}
