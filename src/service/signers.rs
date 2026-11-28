//! The host signing key and the signers a store trusts: creating, showing
//! and rotating the one; adding, removing, listing and following the others.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::core::{attest, fault};
use crate::service::{keys, Workspace};

/// A signing identity, without its secret.
#[derive(Debug, Clone, Serialize)]
pub struct KeyInfo {
    pub path: PathBuf,
    pub public_key: String,
    /// False when the key already existed and was only read.
    pub created: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrustEntry {
    pub name: String,
    pub key: String,
}

/// The host key was replaced; `handover` tells readers about the new one.
#[derive(Debug, Clone, Serialize)]
pub struct KeyRotation {
    pub path: PathBuf,
    pub old_key: String,
    pub new_key: String,
    /// The old secret, kept owner-only beside the new key.
    pub retired: PathBuf,
    /// The statement both keys signed, for everyone who trusts the old key.
    pub handover: PathBuf,
}

/// A trusted signer followed to their new key.
#[derive(Debug, Clone, Serialize)]
pub struct TrustRotation {
    pub name: String,
    pub from: String,
    pub to: String,
    /// False when the new key was already trusted under this name.
    pub added: bool,
    /// The old key is no longer trusted.
    pub revoked: bool,
}

impl Workspace {
    /// Create the host signing key, or report the existing one.
    pub fn key_init() -> Result<KeyInfo> {
        let path = keys::default_path()?;
        let (key, created) = if path.exists() {
            (keys::load(&path)?, false)
        } else {
            (keys::generate(&path)?, true)
        };
        Ok(KeyInfo {
            public_key: key.public_key(),
            path,
            created,
        })
    }

    pub fn key_show() -> Result<KeyInfo> {
        let key = keys::load(&keys::default_path()?)?;
        Ok(KeyInfo {
            public_key: key.public_key(),
            path: key.path().to_path_buf(),
            created: false,
        })
    }

    /// Replace the host key with a new one, and write the handover both keys
    /// sign to `out` (default: `flot-handover-<new key>.json` here). The
    /// handover is written first: if it cannot be, the key is not replaced.
    pub fn key_rotate(out: Option<&Path>) -> Result<KeyRotation> {
        let path = keys::default_path()?;
        let rotation = keys::begin_rotation(&path)?;
        let handover = rotation.handover().clone();
        let out = match out {
            Some(out) => out.to_path_buf(),
            None => std::env::current_dir()?.join(format!(
                "flot-handover-{}.json",
                &handover.to[attest::KEY_PREFIX.len()..][..16]
            )),
        };
        let mut text = serde_json::to_string_pretty(&handover)?;
        text.push('\n');
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out)
            .and_then(|mut file| std::io::Write::write_all(&mut file, text.as_bytes()))
            .with_context(|| format!("cannot write the handover to {}", out.display()))?;
        let retired = rotation.retired_path();
        if let Err(error) = rotation.commit() {
            let _ = std::fs::remove_file(&out);
            return Err(error);
        }
        Ok(KeyRotation {
            path,
            old_key: handover.from,
            new_key: handover.to,
            retired,
            handover: out,
        })
    }

    /// Follow a trusted signer to the key their handover names. The new key
    /// is trusted under the old key's name; the old key stays trusted unless
    /// `revoke`, so the runs it already signed keep their authorship.
    pub fn trust_rotate(&self, path: &Path, revoke: bool) -> Result<TrustRotation> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let handover: attest::Handover = serde_json::from_str(&text)
            .with_context(|| format!("{} is not a flot key handover", path.display()))?;
        let (from, to) = attest::check_handover(&handover).map_err(|error| match error {
            attest::AttestError::BadSignature | attest::AttestError::MalformedSignature => {
                fault::integrity(format!("the handover does not authenticate: {error}"))
            }
            other => anyhow::anyhow!("{} is not a usable handover: {other}", path.display()),
        })?;
        let store = self.store()?;
        let mut trusted = store.trusted_keys()?;
        let name = match (trusted.get(&from), trusted.get(&to)) {
            (Some(old), Some(new)) if old != new => bail!(
                "{} is already trusted as {new}, not as {old}; remove one of them first",
                attest::fingerprint(&to)
            ),
            (Some(name), _) | (None, Some(name)) => name.clone(),
            (None, None) => bail!(
                "the handover is from {}, which this store does not trust, so it vouches for nothing here",
                attest::fingerprint(&from)
            ),
        };
        let added = trusted.insert(to.clone(), name.clone()).is_none();
        if revoke {
            trusted.remove(&from);
        }
        store.save_trusted_keys(&trusted)?;
        Ok(TrustRotation {
            name,
            revoked: !trusted.contains_key(&from),
            from,
            to,
            added,
        })
    }

    /// Believe a signer in this store. Re-adding a name replaces every key it
    /// stood for.
    pub fn trust_add(&self, name: &str, key: &str) -> Result<TrustEntry> {
        let name = name.trim();
        if name.is_empty() {
            bail!("a trusted signer needs a name");
        }
        let key = attest::canonical_key(key).context("cannot trust that key")?;
        let store = self.store()?;
        let mut trusted = store.trusted_keys()?;
        trusted.retain(|_, existing| existing != name);
        trusted.insert(key.clone(), name.to_string());
        store.save_trusted_keys(&trusted)?;
        Ok(TrustEntry {
            name: name.to_string(),
            key,
        })
    }

    /// Stop believing a signer, by name or key.
    pub fn trust_remove(&self, name_or_key: &str) -> Result<TrustEntry> {
        let store = self.store()?;
        let mut trusted = store.trusted_keys()?;
        let wanted = attest::canonical_key(name_or_key).ok();
        let matches: Vec<TrustEntry> = trusted
            .iter()
            .filter(|(key, name)| *name == name_or_key || Some(*key) == wanted.as_ref())
            .map(|(key, name)| TrustEntry {
                name: name.clone(),
                key: key.clone(),
            })
            .collect();
        // After a rotation one name can stand for two keys; say which.
        if matches.len() > 1 {
            bail!(
                "`{name_or_key}` names {} trusted keys; remove one by its key",
                matches.len()
            );
        }
        let found = matches
            .into_iter()
            .next()
            .with_context(|| format!("no trusted signer matches `{name_or_key}`"))?;
        trusted.remove(&found.key);
        store.save_trusted_keys(&trusted)?;
        Ok(found)
    }

    pub fn trust_list(&self) -> Result<Vec<TrustEntry>> {
        let mut entries: Vec<TrustEntry> = self
            .store()?
            .trusted_keys()?
            .into_iter()
            .map(|(key, name)| TrustEntry { name, key })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }
}
