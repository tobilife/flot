//! The digests one run has read, reused while a file provably holds what it
//! held when it was read.
//!
//! A run reads the same files again and again: each step's facts before and
//! after it runs, and every project file before and after a step that may
//! change them. A file whose device, inode, size, mode, modification time and
//! change time are what they were when it was read still has the content it
//! had then, unless it changed within one tick of the file system's clock of
//! that read. So a digest is kept only for a file last changed before a
//! fence: a time the file system itself stamped before the read. Any later
//! change moves the change time past the fence, even one that restores the
//! size and the modification time, and the file is read again. Bazel reuses
//! digests the same way, and git trusts its index so ("racy git").
//!
//! Only where the file system reports a change time: on Unix. Elsewhere every
//! file is read each time it is asked for.

use std::collections::HashMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::core::digest;

/// Digests a run has read. The default reuses nothing.
#[derive(Debug, Default)]
pub struct Digests {
    /// The directory to stamp the fence in, when digests are reused.
    fence_in: Option<PathBuf>,
    /// The file system's time when the first file was read, as seconds and
    /// nanoseconds; `None` when it could not be stamped, and nothing is kept.
    fence: OnceLock<Option<(i64, i64)>>,
    known: Mutex<HashMap<PathBuf, Known>>,
    #[cfg(all(test, unix))]
    reads: std::sync::atomic::AtomicUsize,
}

#[derive(Debug, Clone)]
struct Known {
    stat: Stat,
    digest: String,
    executable: bool,
}

/// What the file system says of a file, without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
struct Stat {
    device: u64,
    inode: u64,
    size: u64,
    mode: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Stat {
    #[cfg(unix)]
    fn of(meta: &Metadata) -> Option<Stat> {
        use std::os::unix::fs::MetadataExt;
        Some(Stat {
            device: meta.dev(),
            inode: meta.ino(),
            size: meta.size(),
            mode: meta.mode(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        })
    }

    #[cfg(not(unix))]
    fn of(_meta: &Metadata) -> Option<Stat> {
        None
    }

    /// When the file was last changed. A modification time can be set past
    /// the change time, so the later of the two.
    fn last_change(&self) -> (i64, i64) {
        self.modified.max(self.changed)
    }
}

impl Digests {
    /// Digests that reuse what this process has read, stamping the fence in
    /// `dir`, which is created when missing. The stamp must be on the file
    /// system of the files read, whose clock it reads: on WSL's `/mnt`
    /// drives that clock is Windows', not this process's.
    pub fn fenced_in(dir: PathBuf) -> Self {
        Digests {
            fence_in: Some(dir),
            ..Digests::default()
        }
    }

    /// The digest of the file at `path` and whether anyone may execute it.
    pub(super) fn digest(&self, path: &Path) -> std::io::Result<(String, bool)> {
        if let Some(known) = self.reusable(path) {
            return Ok((known.digest, known.executable));
        }
        // Before the read, so a change after it lands past the fence.
        let fence = self.fence();
        let (bytes, meta) = super::read_file(path)?;
        #[cfg(all(test, unix))]
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let read = (digest::bytes(&bytes), super::executable(&meta));
        if let (Some(fence), Some(stat)) = (fence, Stat::of(&meta)) {
            if stat.last_change() < fence {
                self.known.lock().expect("no reader panics").insert(
                    path.to_path_buf(),
                    Known {
                        stat,
                        digest: read.0.clone(),
                        executable: read.1,
                    },
                );
            }
        }
        Ok(read)
    }

    /// What was read of `path`, when the file system still says of it what
    /// it said then.
    fn reusable(&self, path: &Path) -> Option<Known> {
        let known = self
            .known
            .lock()
            .expect("no reader panics")
            .get(path)
            .cloned()?;
        let now = Stat::of(&std::fs::metadata(path).ok()?)?;
        (now == known.stat).then_some(known)
    }

    /// The fence, stamped once, at the first file read.
    fn fence(&self) -> Option<(i64, i64)> {
        let dir = self.fence_in.as_ref().filter(|_| cfg!(unix))?;
        *self.fence.get_or_init(|| {
            std::fs::create_dir_all(dir).ok()?;
            let stamp = dir.join("fence");
            std::fs::write(&stamp, b"fence\n").ok()?;
            let stat = Stat::of(&std::fs::metadata(&stamp).ok()?)?;
            Some(stat.modified.min(stat.changed))
        })
    }

    /// How many files were read rather than reused.
    #[cfg(all(test, unix))]
    pub(super) fn reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Wait until the file system's clock is past the last change of `path`,
    /// as it would be for a file edited before a run began: on Linux file
    /// times advance in ticks of the kernel's coarse clock.
    fn settle(dir: &Path, path: &Path) {
        let changed = Stat::of(&std::fs::metadata(path).unwrap())
            .unwrap()
            .last_change();
        let probe = dir.join("probe");
        loop {
            std::fs::write(&probe, "x").unwrap();
            let now = Stat::of(&std::fs::metadata(&probe).unwrap()).unwrap();
            if now.modified.min(now.changed) > changed {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn a_file_is_read_again_once_it_changed_whatever_it_kept() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.txt");
        std::fs::write(&file, "old\n").unwrap();
        settle(dir.path(), &file);
        let digests = Digests::fenced_in(dir.path().join("store"));
        let old = digests.digest(&file).unwrap();
        assert_eq!(old.0, digest::bytes(b"old\n"));
        assert_eq!(digests.digest(&file).unwrap(), old);
        assert_eq!(digests.reads(), 1, "an unchanged file is not read again");

        // The same size, and the modification time put back as it was: only
        // the change time tells.
        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::fs::write(&file, "new\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert_eq!(digests.digest(&file).unwrap().0, digest::bytes(b"new\n"));

        // Replaced by another file of the same size.
        let other = dir.path().join("other.txt");
        std::fs::write(&other, "end\n").unwrap();
        std::fs::rename(&other, &file).unwrap();
        assert_eq!(digests.digest(&file).unwrap().0, digest::bytes(b"end\n"));
    }

    /// A file changed after the fence may change again within the same tick
    /// of the file system's clock and look the same, so it is always read.
    #[test]
    fn a_file_changed_since_the_run_began_is_read_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.txt");
        std::fs::write(&first, "first\n").unwrap();
        let digests = Digests::fenced_in(dir.path().join("store"));
        digests.digest(&first).unwrap();
        let later = dir.path().join("later.txt");
        std::fs::write(&later, "later\n").unwrap();
        let reads = digests.reads();
        digests.digest(&later).unwrap();
        digests.digest(&later).unwrap();
        assert_eq!(digests.reads(), reads + 2);
    }

    #[test]
    fn digests_without_a_fence_read_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.txt");
        std::fs::write(&file, "data\n").unwrap();
        settle(dir.path(), &file);
        let digests = Digests::default();
        digests.digest(&file).unwrap();
        digests.digest(&file).unwrap();
        assert_eq!(digests.reads(), 2);
        assert!(!dir.path().join("store").exists());
    }
}
