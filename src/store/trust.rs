//! The owner's trust list: whose signatures are believed, sealed by this
//! host.

use std::collections::BTreeMap;
use std::fs;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{write_atomic, Store};
use crate::core::fault;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrustFile {
    keys: BTreeMap<String, String>,
    seal: String,
}

impl Store {
    /// Signers the store's owner has chosen to believe: canonical key -> name.
    /// A trust list this host did not write is refused, not believed: one that
    /// arrived with a copied or committed store would decide whose bundles
    /// may stand in for local work.
    pub fn trusted_keys(&self) -> Result<BTreeMap<String, String>> {
        let path = self.root.join("trust.json");
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        let file: TrustFile = serde_json::from_str(&fs::read_to_string(&path)?)
            .map_err(|error| fault::integrity(format!("the trust list is corrupt: {error}")))?;
        if file.seal
            != self
                .host
                .seal("flot-trust-list-v1", &serde_json::json!(file.keys))
        {
            return Err(fault::integrity(format!(
                "the trust list {} was not written by this host's flot; delete it and add the signers you trust again",
                path.display()
            )));
        }
        Ok(file.keys)
    }

    pub fn save_trusted_keys(&self, keys: &BTreeMap<String, String>) -> Result<()> {
        let file = TrustFile {
            keys: keys.clone(),
            seal: self
                .host
                .seal("flot-trust-list-v1", &serde_json::json!(keys)),
        };
        self.ensure()?;
        write_atomic(
            &self.root.join("trust.json"),
            serde_json::to_string_pretty(&file)?.as_bytes(),
        )
    }
}
