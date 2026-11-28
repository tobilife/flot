//! Which files a pattern names: walking the project for its wildcards, past
//! what git ignores unless the pattern spells it out.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::copy_ignore::CopyIgnore;
use super::files::match_options;

/// Which files a wildcard may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Skip what git ignores (`.gitignore` files, `.git/info/exclude`, the
    /// user's global excludes): build trees and caches are not what a step
    /// reads, and hashing them makes every key change.
    Tracked,
    /// Everything: a step's products are often ignored build output.
    Everything,
}

/// The directories before a pattern's first wildcard, and whether it has one.
fn literal_base(pattern: &str) -> (&str, bool) {
    let mut base = "";
    let mut rest = pattern;
    while let Some((part, tail)) = rest.split_once('/') {
        if part.contains(['*', '?', '[']) {
            return (base, true);
        }
        base = &pattern[..pattern.len() - tail.len() - 1];
        rest = tail;
    }
    (base, rest.contains(['*', '?', '[']))
}

/// Files under `root` a pattern names. A wildcard never enters `.git` or the
/// store, and with [`Reach::Tracked`] it skips what git ignores below the
/// pattern's literal directories; a path the pattern spells
/// out is always followed. In a copy of the project, what git ignores is
/// what it ignores in the project (`copy`).
pub(super) fn candidates(
    root: &Path,
    pattern: &str,
    reach: Reach,
    copy: Option<&Arc<CopyIgnore>>,
) -> Result<Vec<PathBuf>, String> {
    candidates_of(root, &[pattern], reach, copy)
        .pop()
        .expect("one pattern has one answer")
}

/// [`candidates`] for each of `patterns`, in order. Patterns whose wildcards
/// start in the same directory share one walk of it: on a network or 9p
/// mount a walk is most of a scan, and `**/*.ts`, `**/*.tsx` and `**/*.json`
/// would otherwise walk the whole project three times.
pub(super) fn candidates_of(
    root: &Path,
    patterns: &[&str],
    reach: Reach,
    copy: Option<&Arc<CopyIgnore>>,
) -> Vec<Result<Vec<PathBuf>, String>> {
    let mut found: Vec<Result<Vec<PathBuf>, String>> = Vec::with_capacity(patterns.len());
    // The patterns to match while walking each start directory.
    let mut walks: BTreeMap<&str, Vec<(usize, glob::Pattern)>> = BTreeMap::new();
    for (index, pattern) in patterns.iter().enumerate() {
        let compiled = match glob::Pattern::new(pattern) {
            Ok(compiled) => compiled,
            Err(error) => {
                found.push(Err(error.to_string()));
                continue;
            }
        };
        let (base, wild) = literal_base(pattern);
        if !wild {
            let path = root.join(pattern);
            found.push(Ok(if path.is_file() {
                vec![path]
            } else {
                Vec::new()
            }));
            continue;
        }
        found.push(Ok(Vec::new()));
        walks.entry(base).or_default().push((index, compiled));
    }
    for (base, matchers) in walks {
        let start = root.join(base);
        if !start.is_dir() {
            continue;
        }
        for path in walk(&start, reach, copy) {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            for (index, compiled) in &matchers {
                if compiled.matches_path_with(relative, match_options()) {
                    if let Ok(paths) = &mut found[*index] {
                        paths.push(path.clone());
                    }
                }
            }
        }
    }
    found
}

/// Every file below `start` a wildcard may reach, in walk order.
fn walk(start: &Path, reach: Reach, copy: Option<&Arc<CopyIgnore>>) -> Vec<PathBuf> {
    // A copy's rules are the project's, applied below; the walker would read
    // the store's.
    let copy = copy.filter(|_| reach == Reach::Tracked).cloned();
    let tracked = reach == Reach::Tracked && copy.is_none();
    let walker = ignore::WalkBuilder::new(start)
        .standard_filters(false)
        .git_ignore(tracked)
        .git_exclude(tracked)
        .git_global(tracked)
        .parents(tracked)
        .require_git(false)
        .follow_links(true)
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let name = entry.file_name();
            let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
            !(is_dir && (name == ".git" || name == crate::store::STORE_DIR))
                && !copy
                    .as_ref()
                    .is_some_and(|rules| rules.ignores(entry.path(), is_dir))
        })
        .build();
    // The walker already knows each entry's type (of its target, since links
    // are followed); asking the file system again costs a round trip per
    // file, which on a network or 9p mount is most of the walk.
    walker
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
        .collect()
}

#[cfg(test)]
mod tests;
