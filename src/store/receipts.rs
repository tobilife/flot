//! Receipts, kept under their id, and found by a full id or a prefix of one.

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use super::{write_atomic, Store};
use crate::core::fault;
use crate::core::receipt::Receipt;

impl Store {
    pub(super) fn receipt_path(&self, receipt_id: &str) -> Result<PathBuf> {
        Ok(self
            .root
            .join("receipts")
            .join(format!("{}.json", Self::hex(receipt_id)?)))
    }

    /// Resolve a full receipt id or an unambiguous hex prefix.
    pub fn find_receipt(&self, spec: &str) -> Result<Receipt> {
        if spec.starts_with("sha256:") {
            return self.load_receipt(spec);
        }
        let mut matches = Vec::new();
        for (_, receipt_id) in self.entries("receipts", ".json")? {
            if receipt_id["sha256:".len()..].starts_with(spec) {
                matches.push(receipt_id);
            }
        }
        match matches.len() {
            0 => bail!("no receipt matches `{spec}`"),
            1 => self.load_receipt(&matches[0]),
            count => bail!("`{spec}` matches {count} receipts; use a longer prefix"),
        }
    }

    pub fn put_receipt(&self, receipt: &Receipt) -> Result<()> {
        if !receipt.is_authentic() {
            bail!(
                "refusing to store receipt {}: its id does not match its body",
                receipt.receipt_id
            );
        }
        let path = self.receipt_path(&receipt.receipt_id)?;
        self.ensure()?;
        write_atomic(&path, serde_json::to_string_pretty(receipt)?.as_bytes())
    }

    pub fn load_receipt(&self, receipt_id: &str) -> Result<Receipt> {
        let path = self.receipt_path(receipt_id)?;
        let text = fs::read_to_string(&path)
            .with_context(|| format!("receipt {receipt_id} is not in the store"))?;
        let receipt: Receipt = serde_json::from_str(&text).map_err(|error| {
            fault::integrity(format!("receipt {receipt_id} is not valid JSON: {error}"))
        })?;
        // The file name is the claim; the content must be the receipt it names.
        if receipt.receipt_id != receipt_id {
            return Err(fault::integrity(format!(
                "the file for receipt {receipt_id} holds receipt {}",
                receipt.receipt_id
            )));
        }
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests;
