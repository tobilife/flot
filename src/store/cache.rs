//! The step-key cache: which receipt may stand in for running a step, in
//! entries sealed by this host.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{write_atomic, Store};
use crate::core::receipt::Receipt;

/// Which receipt answers for a step key, and on whose word.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CacheEntry {
    step_key: String,
    pub(super) receipt_id: String,
    /// Signer keys of the bundle this receipt was imported from. Empty when
    /// this host executed the step. An imported entry is honoured only while
    /// one of these signers is still trusted.
    #[serde(default)]
    vouched_by: Vec<String>,
    seal: String,
}

impl CacheEntry {
    fn body(&self) -> serde_json::Value {
        serde_json::json!({
            "step_key": self.step_key,
            "receipt_id": self.receipt_id,
            "vouched_by": self.vouched_by,
        })
    }
}

/// A cache hit: the receipt, and the signers an imported one depends on.
#[derive(Debug, Clone)]
pub struct Recalled {
    pub receipt: Receipt,
    pub vouched_by: Vec<String>,
}

impl Store {
    fn cache_path(&self, step_key: &str) -> Result<PathBuf> {
        Ok(self
            .root
            .join("cache")
            .join(format!("{}.json", Self::hex(step_key)?)))
    }

    /// Let `receipt_id` answer for `step_key`. `vouched_by` names the signers
    /// of the bundle it came from; empty when this host ran the step.
    pub fn remember(&self, step_key: &str, receipt_id: &str, vouched_by: &[String]) -> Result<()> {
        let mut entry = CacheEntry {
            step_key: step_key.to_string(),
            receipt_id: receipt_id.to_string(),
            vouched_by: vouched_by.to_vec(),
            seal: String::new(),
        };
        entry.seal = self.host.seal("flot-cache-entry-v1", &entry.body());
        self.ensure()?;
        write_atomic(
            &self.cache_path(step_key)?,
            serde_json::to_string_pretty(&entry)?.as_bytes(),
        )
    }

    /// Look up a reusable receipt for a step key.
    ///
    /// A cache hit must be provable: the receipt has to parse, authenticate
    /// against its own id, carry the requested step key, record a settled
    /// success (see `Receipt::is_reusable`), and still have every object it
    /// references present and intact. The entry itself must carry this host's
    /// seal. Anything else is treated as a miss, because a cache that can lie
    /// is worse than no cache, and one that replays a transient failure turns
    /// a flaky step into a permanent one.
    pub fn recall(&self, step_key: &str) -> Result<Option<Recalled>> {
        let path = self.cache_path(step_key)?;
        let Ok(text) = fs::read_to_string(&path) else {
            return Ok(None);
        };
        let Ok(entry) = serde_json::from_str::<CacheEntry>(&text) else {
            return Ok(None);
        };
        if entry.step_key != step_key
            || entry.seal != self.host.seal("flot-cache-entry-v1", &entry.body())
        {
            return Ok(None);
        }
        let Ok(receipt) = self.load_receipt(&entry.receipt_id) else {
            return Ok(None);
        };
        if !receipt.is_authentic()
            || receipt.identity.step_key != step_key
            || !receipt.is_reusable()
        {
            return Ok(None);
        }
        if !receipt
            .objects()
            .into_iter()
            .all(|object| self.read_bytes(object).is_ok())
        {
            return Ok(None);
        }
        Ok(Some(Recalled {
            receipt,
            vouched_by: entry.vouched_by,
        }))
    }
}

#[cfg(test)]
mod tests;
