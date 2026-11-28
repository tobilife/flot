//! One file at a time: its bytes, executable bit and digest, the match
//! options of flot's patterns, and whether a path stays inside the root.

use std::path::Path;

use anyhow::{Context, Result};

use crate::core::digest;

/// A file's bytes and what the file system says of it, both from one open
/// handle: asking by path again costs another round trip, which on a network
/// or 9p mount is as slow as the read. The metadata is taken before the
/// bytes, so a change while they are read is a change after it.
pub(super) fn read_file(path: &Path) -> std::io::Result<(Vec<u8>, std::fs::Metadata)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    Ok((bytes, meta))
}

/// Whether anyone may execute the file `meta` describes.
pub(super) fn executable(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// Whether anyone may execute the file.
pub fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| executable(&meta))
}

/// The match options flot means by a pattern: `*` stays within a directory.
pub fn match_options() -> glob::MatchOptions {
    glob::MatchOptions {
        require_literal_separator: true,
        ..glob::MatchOptions::new()
    }
}

/// Whether `relative` reaches its file without passing through a symbolic
/// link, so reading or writing it cannot leave `root`. Components that do not
/// exist yet are fine: creating them makes real directories.
pub fn within_root(root: &Path, relative: &str) -> Result<bool> {
    let mut current = root.to_path_buf();
    for part in Path::new(relative).components() {
        let std::path::Component::Normal(part) = part else {
            return Ok(false);
        };
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => return Ok(false),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect {}", current.display()))
            }
        }
    }
    Ok(true)
}

/// The digest of the file at `relative` under `root` as it is now, `None`
/// when there is none. Anything but a file there, such as a directory, reads
/// as an empty digest, which no recorded file has.
pub fn content_of(root: &Path, relative: &str) -> Result<Option<String>> {
    let path = root.join(relative);
    match std::fs::metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot inspect {relative}")),
        Ok(meta) if !meta.is_file() => Ok(Some(String::new())),
        Ok(_) => {
            let bytes = std::fs::read(&path).with_context(|| format!("cannot read {relative}"))?;
            Ok(Some(digest::bytes(&bytes)))
        }
    }
}
