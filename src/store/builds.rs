//! The copy of the work proofs run in and the directory they build in: taken
//! by one proof at a time, and pruned once no proof will use them.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::lock::{open_lock, try_lock};
use super::Store;

/// Where proofs on copies of the work build, under the store.
const BUILDS: &str = "builds";

/// How long the copies' build is kept after a proof last used it.
const BUILDS_KEPT: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// The copy of the work proofs run in and the directory they build in, held
/// by one proof at a time, in this flot or another: one copy and one build
/// for all of them, kept from one proof to the next, so a proof rebuilds
/// only what changed since the last. Released when dropped, which marks when
/// they were last used.
pub struct Builds {
    root: PathBuf,
    file: fs::File,
    /// When the last holder released them.
    released: std::time::SystemTime,
}

impl Builds {
    /// Where the proofs build.
    pub fn target(&self) -> PathBuf {
        self.root.join("target")
    }

    /// The copy of the work.
    pub fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    /// What the copy's files held when last read, for the next proof.
    pub fn digests(&self) -> PathBuf {
        self.root.join("work.json")
    }

    /// Where to stamp the copy's digests' fence: on the copy's file system.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The time to give a file written in the copy: past everything the last
    /// holder built, by more than the coarsest file system's two seconds, so
    /// a build that goes by times takes it as changed.
    pub fn stamp(&self) -> std::time::SystemTime {
        std::time::SystemTime::now().max(self.released + std::time::Duration::from_secs(2))
    }
}

impl Drop for Builds {
    fn drop(&mut self) {
        let _ = self.file.set_modified(std::time::SystemTime::now());
    }
}

impl Store {
    /// Take the directory proofs on copies of the work build in, waiting
    /// while another proof holds it until `stopped` says to give up.
    pub fn builds(&self, stopped: &dyn Fn() -> bool) -> Result<Builds> {
        self.ensure()?;
        let root = self.root.join(BUILDS);
        fs::create_dir_all(&root).with_context(|| format!("cannot create {}", root.display()))?;
        let file = open_lock(&root.join("lock"))?;
        while !try_lock(&file).context("cannot lock the copies' build")? {
            if stopped() {
                bail!("the run stopped while another proof built in a copy of the work");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let released = file
            .metadata()
            .and_then(|meta| meta.modified())
            .with_context(|| format!("cannot read {}", root.join("lock").display()))?;
        Ok(Builds {
            root,
            file,
            released,
        })
    }

    /// Remove builds no proof will use: those of another layout, and the
    /// copy and its build once no proof has used them for [`BUILDS_KEPT`].
    /// Nothing is removed while a proof holds them, or anything else holds
    /// their lock, such as a process another thread is starting, which has
    /// the lock's file open until it runs its program; `false` then. What
    /// cannot be removed now is tried again next time.
    pub fn prune_builds(&self) -> bool {
        let root = self.root.join(BUILDS);
        let Ok(entries) = fs::read_dir(&root) else {
            return true;
        };
        let Ok(file) = open_lock(&root.join("lock")) else {
            return true;
        };
        if !matches!(try_lock(&file), Ok(true)) {
            return false;
        }
        let stale = file
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|used| used.elapsed().is_ok_and(outlived));
        for entry in entries.flatten() {
            let name = entry.file_name();
            let kept = ["target", "work", "work.json", "fence"];
            if name == "lock" || (!stale && kept.iter().any(|kept| name == *kept)) {
                continue;
            }
            let path = entry.path();
            let _ = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
        }
        true
    }
}

/// Whether builds unused for `age` are no longer kept.
fn outlived(age: std::time::Duration) -> bool {
    age > BUILDS_KEPT
}

#[cfg(test)]
mod tests;
