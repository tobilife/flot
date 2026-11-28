//! The store lock runs and gc hold, and the file locks it and the copies'
//! build are taken with.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use super::Store;
use crate::core::fault;

/// Held while a run or an import writes to the store, and exclusively by gc,
/// so a collection never removes evidence a run has not recorded yet.
pub struct StoreLock {
    _file: fs::File,
}

impl Store {
    /// Take the store lock: shared for writers that only add (runs, imports),
    /// exclusive for gc, which fails at once rather than wait for a run.
    pub fn lock(&self, exclusive: bool) -> Result<StoreLock> {
        self.ensure()?;
        let file = open_lock(&self.root.join("lock"))?;
        let locked = if exclusive {
            try_lock(&file)
        } else {
            file.lock_shared().map(|()| true)
        };
        match locked {
            Ok(true) => Ok(StoreLock { _file: file }),
            Ok(false) => Err(fault::environment(
                "the store is in use by a running flot; try again when it finishes",
            )),
            Err(error) => Err(error).context("cannot lock the store"),
        }
    }
}

/// A lock file, created if it is not there yet.
pub(super) fn open_lock(path: &Path) -> Result<fs::File> {
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))
}

/// Lock all of `file` exclusively until it is closed, or `Ok(false)` at once
/// when another holder keeps a lock on it. Each open file is a holder of its
/// own, also within one process.
pub(super) fn try_lock(file: &fs::File) -> std::io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(fs::TryLockError::WouldBlock) => Ok(false),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests;
