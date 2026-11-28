//! Secret files readable only by their owner, written whole: complete
//! before their name exists, or replacing the old content in one step.

use std::path::{Path, PathBuf};

/// Create `path` holding `data`, readable only by its owner, and never
/// replace an existing file. The content is complete before the name exists
/// (written aside, then hard-linked into place), so a flot starting at the
/// same moment either finds no file or the whole of it, never a torn one.
pub(super) fn write_owner_only(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let aside = write_aside(path, data)?;
    let placed = std::fs::hard_link(&aside, path);
    let _ = std::fs::remove_file(&aside);
    placed
}

/// Replace whatever is at `path` with `data`, readable only by its owner, in
/// one step: a reader sees the old content or the new, never a mix.
pub(super) fn replace_owner_only(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let aside = write_aside(path, data)?;
    let placed = std::fs::rename(&aside, path);
    if placed.is_err() {
        let _ = std::fs::remove_file(&aside);
    }
    placed
}

/// `data` in a fresh owner-only file beside `path`, synced to disk.
fn write_aside(path: &Path, data: &[u8]) -> std::io::Result<PathBuf> {
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
    match written {
        Ok(()) => Ok(aside),
        Err(error) => {
            let _ = std::fs::remove_file(&aside);
            Err(error)
        }
    }
}
