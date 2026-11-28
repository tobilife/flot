//! Attestations recorded for a run, one per signer key.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use super::{write_atomic, Store};
use crate::core::attest::Attestation;
use crate::core::fault;

impl Store {
    fn attestation_path(&self, run_id: &str) -> Result<PathBuf> {
        self.run_file("attestations", run_id)
    }

    /// Attestations recorded for a run. None recorded is an empty list.
    pub fn attestations(&self, run_id: &str) -> Result<Vec<Attestation>> {
        let path = self.attestation_path(run_id)?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&path)?;
        serde_json::from_str(&text).map_err(|error| {
            fault::integrity(format!(
                "attestations for run {run_id} are corrupt: {error}"
            ))
        })
    }

    /// Record attestations for a run, keeping one per signer key.
    pub fn add_attestations(&self, run_id: &str, attestations: &[Attestation]) -> Result<()> {
        let mut known = self.attestations(run_id)?;
        self.ensure()?;
        for attestation in attestations {
            known.retain(|existing| existing.key != attestation.key);
            known.push(attestation.clone());
        }
        known.sort_by(|a, b| a.key.cmp(&b.key));
        write_atomic(
            &self.attestation_path(run_id)?,
            serde_json::to_string_pretty(&known)?.as_bytes(),
        )
    }
}
