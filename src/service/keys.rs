//! The host signing key.
//!
//! One Ed25519 key per user, outside any project, so a person signs every run
//! they ship with the same identity. The file holds the 32-byte secret as hex
//! and is created owner-only; a key file other users can read is refused, the
//! way ssh refuses a world-readable private key. The secret never leaves this
//! module except as signatures.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::core::attest::{self, Attestation};
use crate::core::receipt::RunManifest;
use crate::store::HostSecret;

mod owner_only;
mod rotation;

pub use rotation::{begin_rotation, Rotation};

use owner_only::write_owner_only;

/// Overrides the key location. Tests and CI point this at a scratch path.
pub const KEY_ENV: &str = "FLOT_SIGNING_KEY";

pub struct SigningKeyFile {
    path: PathBuf,
    secret: [u8; 32],
}

impl SigningKeyFile {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn public_key(&self) -> String {
        attest::public_key_of(&self.secret)
    }

    pub fn sign(&self, manifest: &RunManifest) -> Attestation {
        attest::sign(&self.secret, manifest)
    }
}

/// Where the key lives: `$FLOT_SIGNING_KEY`, else
/// `$XDG_CONFIG_HOME/flot/signing.key`, else `%APPDATA%\flot\signing.key` on
/// Windows and `~/.config/flot/signing.key` elsewhere.
pub fn default_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(KEY_ENV) {
        return Ok(PathBuf::from(path));
    }
    Ok(config_dir()?.join("flot").join("signing.key"))
}

/// The public key of the key at [`default_path`], as `flot key show` prints
/// it, or `None` when the path cannot be resolved or the key cannot be
/// loaded. The error is dropped on purpose: this key only labels a signer,
/// and a label must not make `verify` fail. `flot doctor` reports an
/// unusable key.
pub fn host_public_key() -> Option<String> {
    let path = default_path().ok()?;
    load(&path).ok().map(|key| key.public_key())
}

#[cfg(not(test))]
fn config_dir() -> Result<PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    if let Some(dir) = set("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(dir));
    }
    #[cfg(windows)]
    if let Some(dir) = set("APPDATA") {
        return Ok(PathBuf::from(dir));
    }
    Ok(
        PathBuf::from(set("HOME").context("neither $FLOT_SIGNING_KEY nor $HOME is set")?)
            .join(".config"),
    )
}

/// Unit tests never read or create secrets in the real home directory.
#[cfg(test)]
fn config_dir() -> Result<PathBuf> {
    Ok(std::env::temp_dir().join("flot-unit-tests"))
}

/// The host secret that seals each store's cache and trust list, kept beside
/// the signing key (`host.secret` in the same directory) and created
/// owner-only on first use. It is not a signing identity: it only lets this
/// host recognise the records it wrote, so losing it costs cache hits and the
/// trust list, never evidence.
pub fn host_secret() -> Result<HostSecret> {
    let key = default_path()?;
    let path = key
        .parent()
        .map(|dir| dir.join("host.secret"))
        .context("the signing key path has no directory")?;
    if !path.exists() {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret)
            .map_err(|error| anyhow::anyhow!("the OS random source failed: {error}"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match write_owner_only(&path, hex::encode(secret).as_bytes()) {
            Ok(()) => return Ok(HostSecret::from_bytes(secret)),
            // Another flot created it first; read theirs.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("cannot write {}", path.display()))
            }
        }
    }
    refuse_shared(&path)?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let secret: [u8; 32] = hex::decode(text.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .with_context(|| format!("{} is not a flot host secret", path.display()))?;
    Ok(HostSecret::from_bytes(secret))
}

/// Create a new key. Refuses to replace an existing one: losing a key orphans
/// every trust entry that names it.
pub fn generate(path: &Path) -> Result<SigningKeyFile> {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret)
        .map_err(|error| anyhow::anyhow!("the OS random source failed: {error}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_owner_only(path, hex::encode(secret).as_bytes()).with_context(|| {
        if path.exists() {
            format!(
                "{} already exists; flot will not replace a signing key",
                path.display()
            )
        } else {
            format!("cannot write {}", path.display())
        }
    })?;
    Ok(SigningKeyFile {
        path: path.to_path_buf(),
        secret,
    })
}

pub fn load(path: &Path) -> Result<SigningKeyFile> {
    if !path.exists() {
        bail!(
            "no signing key at {}; create one with `flot key init`",
            path.display()
        );
    }
    refuse_shared(path)?;
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read signing key {}", path.display()))?;
    let secret: [u8; 32] = hex::decode(text.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .with_context(|| format!("{} is not a flot signing key", path.display()))?;
    Ok(SigningKeyFile {
        path: path.to_path_buf(),
        secret,
    })
}

#[cfg(unix)]
fn refuse_shared(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        bail!(
            "secret {} is readable by other users (mode {:o}); run `chmod 600 {}`",
            path.display(),
            mode & 0o777,
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn refuse_shared(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests;
