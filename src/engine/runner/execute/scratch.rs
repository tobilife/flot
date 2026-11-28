//! Scratch directories in the store's `tmp`, each removed when it is done
//! with.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A directory in the store's `tmp` for one confined execution or one copy
/// of the work, removed when it is done with.
pub(super) struct Scratch {
    pub(super) path: PathBuf,
}

impl Scratch {
    /// A directory named for what it holds, `kind`.
    pub(super) fn new(store_root: &Path, kind: &str) -> Result<Self> {
        let path = store_root.join("tmp").join(format!(
            "{kind}-{}-{}",
            std::process::id(),
            SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("tmp"))
            .with_context(|| format!("cannot create {}", path.display()))?;
        Ok(Scratch { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
