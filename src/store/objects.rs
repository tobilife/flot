//! Content-addressed objects: bytes kept under their digest, and the
//! observations of a step kept as objects.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::{write_atomic, Store};
use crate::core::digest;
use crate::core::fault;
use crate::core::receipt::Observed;

impl Store {
    fn object_path(&self, object: &str) -> Result<PathBuf> {
        let hex = Self::hex(object)?;
        Ok(self.root.join("objects").join(&hex[..2]).join(hex))
    }

    /// Store bytes under their digest. Writing the same bytes twice is a no-op.
    /// An existing object is re-read first: one that no longer matches its
    /// name (a torn write, bit rot, tampering) is replaced, not trusted.
    pub fn put_bytes(&self, data: &[u8]) -> Result<String> {
        let object = digest::bytes(data);
        let path = self.object_path(&object)?;
        if fs::read(&path).is_ok_and(|existing| digest::bytes(&existing) == object) {
            return Ok(object);
        }
        self.ensure()?;
        fs::create_dir_all(path.parent().expect("object path has a parent"))?;
        write_atomic(&path, data)?;
        Ok(object)
    }

    /// Keep one run's observation of a step, named by its digest.
    pub fn put_observation(&self, observed: &Observed) -> Result<String> {
        self.put_bytes(digest::canonical_json(observed).as_bytes())
    }

    pub fn load_observation(&self, object: &str) -> Result<Observed> {
        serde_json::from_slice(&self.read_bytes(object)?).map_err(|error| {
            fault::integrity(format!("object {object} is not an observation: {error}"))
        })
    }

    pub fn has_object(&self, object: &str) -> bool {
        self.object_path(object)
            .map(|path| path.exists())
            .unwrap_or(false)
    }

    pub fn read_bytes(&self, object: &str) -> Result<Vec<u8>> {
        let path = self.object_path(object)?;
        let data = fs::read(&path)
            .with_context(|| format!("object {object} is missing from the store"))?;
        let actual = digest::bytes(&data);
        if actual != object {
            return Err(fault::integrity(format!(
                "object {object} does not match its content (found {actual})"
            )));
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests;
