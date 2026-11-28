//! Scans: the files a step's globs match, each recorded with the digest of
//! its content and its executable bit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;

use crate::core::receipt::FileDigest;

use super::copy_ignore::CopyIgnore;
use super::files::{executable, match_options, read_file, within_root};
use super::walk::{candidates_of, Reach};
use super::{recorded_modes, Digests};

/// Files matched by a set of globs.
#[derive(Debug, Default)]
pub struct Scan {
    /// Sorted by path, each with the digest `keep` returned for its bytes.
    pub files: Vec<FileDigest>,
    /// Patterns that matched no readable file.
    pub unmatched: Vec<String>,
    /// Invalid patterns and unreadable files.
    pub problems: Vec<String>,
}

/// Resolve globs, relative to `root`, handing each matched file's bytes to
/// `keep`, which returns the digest to record for it. Files are read one at a
/// time so a large tree is never held in memory at once.
pub fn scan_globs<E>(
    root: &Path,
    patterns: &[String],
    kind: &str,
    reach: Reach,
    mut keep: impl FnMut(&[u8]) -> Result<String, E>,
) -> Result<Scan, E> {
    scan_in(
        root,
        None,
        patterns,
        kind,
        reach,
        true,
        Keep::With(&mut keep),
    )
}

/// How a scan records each file it reads.
pub(super) enum Keep<'a, E> {
    /// The digest of its content, through `Digests`, which may already
    /// hold it. Files are read several at a time: on a network or 9p mount
    /// each read waits on a round trip, and reading one at a time spends
    /// most of the scan waiting.
    Digest(&'a Digests),
    /// What the callback returns for its bytes, one file at a time, so a
    /// large tree is never held in memory at once.
    With(&'a mut dyn FnMut(&[u8]) -> Result<String, E>),
}

/// [`scan_globs`] in `root`, a copy of `origin` when that is given, whose
/// ignore rules and recorded executable bits then apply. Without `modes`,
/// every file keeps the bit the file system shows.
pub(super) fn scan_in<E>(
    root: &Path,
    origin: Option<&Path>,
    patterns: &[String],
    kind: &str,
    reach: Reach,
    modes: bool,
    keep: Keep<'_, E>,
) -> Result<Scan, E> {
    let mut scan = Scan::default();
    let copy = origin
        .filter(|_| reach == Reach::Tracked)
        .map(|origin| Arc::new(CopyIgnore::new(root, origin)));
    // Each valid pattern with the files it matched, and every file once, in
    // the order first matched.
    let mut matches: Vec<(&String, Vec<String>)> = Vec::new();
    let mut unique: Vec<(String, PathBuf)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let listed = candidates_of(
        root,
        &patterns.iter().map(String::as_str).collect::<Vec<_>>(),
        reach,
        copy.as_ref(),
    );
    for (pattern, entries) in patterns.iter().zip(listed) {
        let entries = match entries {
            Ok(entries) => entries,
            Err(error) => {
                scan.problems
                    .push(format!("{kind} pattern `{pattern}` is invalid: {error}"));
                continue;
            }
        };
        let mut matched = Vec::new();
        for entry in entries {
            let relative = entry
                .strip_prefix(root)
                .unwrap_or(&entry)
                .to_string_lossy()
                .replace('\\', "/");
            // The store is flot's own bookkeeping, rewritten by every run;
            // it is never a step's input, proof or product.
            if relative == crate::store::STORE_DIR
                || relative.starts_with(&format!("{}/", crate::store::STORE_DIR))
            {
                continue;
            }
            if seen.insert(relative.clone()) {
                unique.push((relative.clone(), entry));
            }
            matched.push(relative);
        }
        matches.push((pattern, matched));
    }
    // What a step reads is keyed as git records it where git does not trust
    // the file system; what it produces keeps the bit it was written with,
    // which reuse restores. Asking git costs two processes, so only a scan
    // that found files asks.
    let recorded = match reach {
        Reach::Tracked if modes && !unique.is_empty() => recorded_modes(origin.unwrap_or(root)),
        _ => None,
    };
    let read: Vec<std::io::Result<(String, bool)>> = match keep {
        Keep::Digest(digests) => digest_all(&unique, digests),
        Keep::With(keep) => {
            let mut read = Vec::with_capacity(unique.len());
            for (_, path) in &unique {
                read.push(match read_file(path) {
                    Ok((bytes, meta)) => Ok((keep(&bytes)?, executable(&meta))),
                    Err(error) => Err(error),
                });
            }
            read
        }
    };
    let mut files: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for ((relative, _), read) in unique.into_iter().zip(read) {
        match read {
            Ok((digest, mode)) => {
                let executable = recorded
                    .as_ref()
                    .and_then(|modes| modes.get(&relative).copied())
                    .unwrap_or(mode);
                files.insert(relative, (digest, executable));
            }
            Err(error) => scan
                .problems
                .push(format!("{kind} `{relative}` could not be read: {error}")),
        }
    }
    for (pattern, matched) in matches {
        if !matched.iter().any(|relative| files.contains_key(relative)) {
            scan.unmatched.push(pattern.clone());
        }
    }
    scan.files = files
        .into_iter()
        .map(|(path, (digest, executable))| FileDigest {
            path,
            digest,
            executable,
        })
        .collect();
    Ok(scan)
}

/// The digest and executable bit of each file, in order, read by up to eight
/// threads at once. Each thread holds one file at a time.
pub(super) fn digest_all(
    files: &[(String, PathBuf)],
    digests: &Digests,
) -> Vec<std::io::Result<(String, bool)>> {
    let one = |(_, path): &(String, PathBuf)| digests.digest(path);
    // A thread costs more than reading a few small files.
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(8)
        .min(files.len() / 8)
        .max(1);
    if threads == 1 {
        return files.iter().map(one).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut read: Vec<Option<std::io::Result<(String, bool)>>> =
        std::iter::repeat_with(|| None).take(files.len()).collect();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(file) = files.get(index) else {
                            return done;
                        };
                        done.push((index, one(file)));
                    }
                })
            })
            .collect();
        for worker in workers {
            for (index, result) in worker.join().expect("a digest thread does not panic") {
                read[index] = Some(result);
            }
        }
    });
    read.into_iter()
        .map(|result| result.expect("every file was read"))
        .collect()
}

/// Collect what a step produced, storing each file through `keep`. A file
/// reached through a symbolic link is not the step's to record: it may live
/// anywhere, and reuse would write back through the same link.
pub fn scan_outputs(
    root: &Path,
    patterns: &[String],
    keep: impl FnMut(&[u8]) -> Result<String>,
) -> Result<Scan> {
    let mut scan = scan_globs(root, patterns, "output", Reach::Everything, keep)?;
    let mut linked = Vec::new();
    for file in &scan.files {
        if !within_root(root, &file.path)? {
            linked.push(file.path.clone());
        }
    }
    if !linked.is_empty() {
        scan.files.retain(|file| !linked.contains(&file.path));
        for path in &linked {
            scan.problems.push(format!(
                "output {path} is reached through a symbolic link and was not recorded"
            ));
        }
        for pattern in patterns {
            let options = match_options();
            let still_matched = glob::Pattern::new(pattern).is_ok_and(|compiled| {
                scan.files
                    .iter()
                    .any(|file| compiled.matches_with(&file.path, options))
            });
            if !still_matched && !scan.unmatched.contains(pattern) {
                scan.unmatched.push(pattern.clone());
            }
        }
    }
    Ok(scan)
}

/// Every file a wildcard reaches under `root` (so not what git ignores, and
/// never `.git` or the store), with the digest of its content. `root` is the
/// project `origin` or a copy of it, read with the project's ignore rules.
pub fn snapshot(root: &Path, origin: &Path, digests: &Digests) -> Scan {
    scan_in(
        root,
        (root != origin).then_some(origin),
        &["**/*".to_string()],
        "project file",
        Reach::Tracked,
        false,
        Keep::<std::convert::Infallible>::Digest(digests),
    )
    .unwrap_or_else(|never| match never {})
}

#[cfg(test)]
mod tests;
