//! The `.flot` store: content-addressed objects, receipts, the step-key cache
//! run manifests, the attestations over them, and the owner's trust list.
//!
//! Nothing in here is a database. Evidence (objects, receipts, observations,
//! run manifests, attestations) authenticates itself: objects and receipts are
//! named by digests of their content, and a manifest is checked against its
//! receipts. That part can be copied, committed or shipped and still be
//! audited anywhere.
//!
//! Two parts are decisions, not evidence: the step-key cache (which receipts
//! may stand in for running a step) and the trust list (whose signatures are
//! believed). Both are sealed with a secret that never leaves this host, so a
//! copied, committed or downloaded store cannot carry them in. A record whose
//! seal does not verify is ignored (cache) or refused (trust list).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};

use crate::core::digest;

mod attestations;
mod builds;
mod cache;
mod gc;
mod lock;
mod objects;
mod receipts;
mod refusals;
mod runs;
mod trust;

pub use builds::Builds;
pub use cache::Recalled;
pub use gc::GcReport;
pub use lock::StoreLock;
pub use runs::MIN_RUN_PREFIX;

pub const STORE_DIR: &str = ".flot";

/// Layout and record format of a store. A store written in another format is
/// refused whole, with how to start over, rather than read half-right: flot
/// does not migrate evidence, because a migrated receipt is not what was
/// observed.
pub const STORE_FORMAT: u32 = 3;

/// A secret that never leaves this host. It seals the store's decisions
/// (cache entries, the trust list) so that only this host's flot can write
/// records it will act on.
#[derive(Clone)]
pub struct HostSecret([u8; 32]);

impl HostSecret {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        HostSecret(bytes)
    }

    fn seal(&self, domain: &str, value: &serde_json::Value) -> String {
        let message = digest::canonical_json(&serde_json::json!({
            "domain": domain,
            "value": value,
        }));
        digest::mac(&self.0, message.as_bytes())
    }
}

impl std::fmt::Debug for HostSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostSecret(..)")
    }
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    host: HostSecret,
}

impl Store {
    /// Open the store under `flow_root/.flot`. Nothing is created until the
    /// first write, so reading, or refusing a bundle, leaves no trace.
    pub fn open(flow_root: &Path, host: HostSecret) -> Result<Self> {
        let root = flow_root.join(STORE_DIR);
        let format_path = root.join("FORMAT");
        match fs::read_to_string(&format_path) {
            Ok(text) if text.trim() == STORE_FORMAT.to_string() => {}
            Ok(text) => bail!(
                "{} was written in store format {} and this flot reads format {STORE_FORMAT}; move it aside to start a new store",
                root.display(),
                text.trim()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let holds_runs = fs::read_dir(root.join("runs"))
                    .map(|mut entries| entries.next().is_some())
                    .unwrap_or(false);
                if holds_runs {
                    bail!(
                        "{} was written by an older flot (no store format); move it aside to start a new store",
                        root.display()
                    );
                }
            }
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", format_path.display()))
            }
        }
        Ok(Store { root, host })
    }

    /// Create the store on disk if it is not there yet.
    pub fn ensure(&self) -> Result<()> {
        let format_path = self.root.join("FORMAT");
        if format_path.exists() {
            return Ok(());
        }
        fs::create_dir_all(&self.root)
            .with_context(|| format!("cannot create {}", self.root.display()))?;
        // The store is this host's evidence, shared through bundles; a
        // committed copy would only be refused elsewhere. Keep it out of git
        // without asking the user to edit their own ignore file.
        write_atomic(
            &self.root.join(".gitignore"),
            b"# Created by flot. The store is local; share runs with `flot bundle`.\n*\n",
        )?;
        write_atomic(&format_path, STORE_FORMAT.to_string().as_bytes())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn hex(digest: &str) -> Result<&str> {
        let hex = digest
            .strip_prefix("sha256:")
            .with_context(|| format!("unsupported digest form `{digest}`"))?;
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("digest `{digest}` is not a sha256 hex digest");
        }
        Ok(hex)
    }

    /// `(path, digest)` pairs for digest-named files in one store directory.
    fn entries(&self, directory: &str, suffix: &str) -> Result<Vec<(PathBuf, String)>> {
        let dir = self.root.join(directory);
        let mut found = Vec::new();
        if !dir.exists() {
            return Ok(found);
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if let Some(hex) = name.strip_suffix(suffix) {
                found.push((path, format!("sha256:{hex}")));
            }
        }
        Ok(found)
    }
}

/// Monotonic suffix source so two concurrent writers of the same digest cannot
/// share a temporary file and tear each other's bytes.
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!(
        "tmp{}-{}",
        std::process::id(),
        WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    {
        use std::io::Write;
        let mut file = fs::File::create(&temporary)
            .with_context(|| format!("cannot write {}", temporary.display()))?;
        file.write_all(data)
            .with_context(|| format!("cannot write {}", temporary.display()))?;
        // On disk before it has a name, so a crash never leaves a named file
        // with torn content.
        file.sync_all()
            .with_context(|| format!("cannot write {}", temporary.display()))?;
    }
    fs::rename(&temporary, path).with_context(|| format!("cannot place {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests;
