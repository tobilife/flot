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
/// `$XDG_CONFIG_HOME/flot/signing.key`, else `~/.config/flot/signing.key`.
pub fn default_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(KEY_ENV) {
        return Ok(PathBuf::from(path));
    }
    Ok(config_dir()?.join("flot").join("signing.key"))
}

#[cfg(not(test))]
fn config_dir() -> Result<PathBuf> {
    Ok(match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(
            std::env::var_os("HOME").context("neither $FLOT_SIGNING_KEY nor $HOME is set")?,
        )
        .join(".config"),
    })
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

/// Create `path` holding `data`, readable only by its owner, and never
/// replace an existing file. The content is complete before the name exists
/// (written aside, then hard-linked into place), so a flot starting at the
/// same moment either finds no file or the whole of it, never a torn one.
fn write_owner_only(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    static ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let aside = path.with_file_name(format!(
        ".{}.{}-{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("secret"),
        std::process::id(),
        ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&aside).and_then(|mut file| {
        file.write_all(data)?;
        file.sync_all()
    });
    let placed = written.and_then(|()| std::fs::hard_link(&aside, path));
    let _ = std::fs::remove_file(&aside);
    placed
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
mod tests {
    use super::*;

    #[test]
    fn a_generated_key_loads_back_to_the_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/signing.key");
        let created = generate(&path).unwrap();
        assert_eq!(load(&path).unwrap().public_key(), created.public_key());
    }

    #[test]
    fn two_generated_keys_differ() {
        let dir = tempfile::tempdir().unwrap();
        let a = generate(&dir.path().join("a.key")).unwrap();
        let b = generate(&dir.path().join("b.key")).unwrap();
        assert_ne!(a.public_key(), b.public_key());
    }

    #[test]
    fn an_existing_key_is_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signing.key");
        let original = generate(&path).unwrap().public_key();
        let error = generate(&path).err().expect("second generate must fail");
        assert!(
            format!("{error:#}").contains("will not replace"),
            "{error:#}"
        );
        assert_eq!(load(&path).unwrap().public_key(), original);
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_owner_only_and_a_shared_one_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signing.key");
        generate(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = load(&path).err().expect("a shared key must be refused");
        assert!(
            error.to_string().contains("readable by other users"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_or_garbled_key_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signing.key");
        assert!(load(&path)
            .err()
            .unwrap()
            .to_string()
            .contains("flot key init"));
        write_owner_only(&path, b"not hex").unwrap();
        assert!(load(&path)
            .err()
            .unwrap()
            .to_string()
            .contains("not a flot signing key"));
    }

    #[test]
    fn concurrent_creators_never_see_a_torn_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.secret");
        let data: Vec<Vec<u8>> = (0..16u8).map(|n| vec![b'a' + n; 4096]).collect();
        let results: Vec<std::io::Result<()>> = std::thread::scope(|scope| {
            let handles: Vec<_> = data
                .iter()
                .map(|bytes| scope.spawn(|| write_owner_only(&path, bytes)))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        let winners = results.iter().filter(|result| result.is_ok()).count();
        assert_eq!(winners, 1);
        assert!(results.iter().flatten().count() == 1);
        for result in &results {
            if let Err(error) = result {
                assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
            }
        }
        let content = std::fs::read(&path).unwrap();
        assert!(
            data.contains(&content),
            "the file is one writer's whole content"
        );
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "no aside files are left behind");
    }
}
