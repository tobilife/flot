//! Rotating the host key: a successor made and vouched for by a handover
//! both keys sign, then put in place with the old secret kept beside it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::owner_only::replace_owner_only;
use super::{load, SigningKeyFile};
use crate::core::attest::{self, Handover};

/// A successor key, made and vouched for, but not yet in place. Nothing on
/// disk changes until [`Rotation::commit`], so the caller can first store the
/// handover that tells readers about the new key.
pub struct Rotation {
    current: SigningKeyFile,
    next: [u8; 32],
    handover: Handover,
}

impl Rotation {
    pub fn handover(&self) -> &Handover {
        &self.handover
    }

    /// Where the old secret is kept once the new key is in place.
    pub fn retired_path(&self) -> PathBuf {
        let public = self.current.public_key();
        let short = &public[attest::KEY_PREFIX.len()..][..16];
        let name = self
            .current
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("signing.key");
        self.current
            .path
            .with_file_name(format!("{name}.retired-{short}"))
    }

    /// Put the new key in place of the old one. The old secret is kept
    /// beside it (owner-only, [`Rotation::retired_path`]) rather than
    /// destroyed: its signatures stay valid, and only its owner can decide
    /// it is no longer needed.
    pub fn commit(self) -> Result<SigningKeyFile> {
        let retired = self.retired_path();
        let path = self.current.path;
        std::fs::hard_link(&path, &retired).with_context(|| {
            format!(
                "cannot keep the old key as {}; nothing was changed",
                retired.display()
            )
        })?;
        if let Err(error) = replace_owner_only(&path, hex::encode(self.next).as_bytes()) {
            let _ = std::fs::remove_file(&retired);
            return Err(error).with_context(|| {
                format!(
                    "cannot write the new key to {}; nothing was changed",
                    path.display()
                )
            });
        }
        Ok(SigningKeyFile {
            path,
            secret: self.next,
        })
    }
}

/// Make a successor for the key at `path` and the handover both keys sign.
pub fn begin_rotation(path: &Path) -> Result<Rotation> {
    let current = load(path)?;
    let mut next = [0u8; 32];
    getrandom::fill(&mut next)
        .map_err(|error| anyhow::anyhow!("the OS random source failed: {error}"))?;
    let handover = attest::hand_over(&current.secret, &next);
    Ok(Rotation {
        current,
        next,
        handover,
    })
}
