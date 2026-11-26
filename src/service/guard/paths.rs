//! Paths as the guard compares them: folded where names ignore case,
//! resolved through links, named below a base, and the harness files that
//! decide whether proofs are checked.

use std::path::{Path, PathBuf};

/// Whether this platform's file systems usually ignore case, so that
/// `FLOT.TOML` is the flow file.
pub(super) const FOLDS_CASE: bool = cfg!(any(windows, target_os = "macos"));

/// How many entries the guard reads below a directory a call changes, for
/// what no flow names in advance.
pub(super) const GUARD_WALK_LIMIT: usize = 5000;

/// `text` as names are compared on this platform.
pub(super) fn folded(text: &str) -> String {
    if FOLDS_CASE {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

/// `path` below `base`, or `base` itself as ``, `/`-separated. With `fold`,
/// a component matches whatever its case.
pub(super) fn strip_base(path: &Path, base: &Path, fold: bool) -> Option<String> {
    let mut parts = path.components();
    for expected in base.components() {
        let part = parts.next()?;
        let same = part == expected
            || (fold
                && part.as_os_str().to_string_lossy().to_lowercase()
                    == expected.as_os_str().to_string_lossy().to_lowercase());
        if !same {
            return None;
        }
    }
    Some(
        parts
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// `path` below the first of `bases` that holds it, `/`-separated.
pub(super) fn below(path: &Path, bases: &[PathBuf]) -> Option<String> {
    bases
        .iter()
        .find_map(|base| strip_base(path, base, FOLDS_CASE))
}

/// `dir` as named, and as resolved when that differs.
pub(super) fn spellings(dir: &Path) -> Vec<PathBuf> {
    let named = normalize(dir);
    let resolved = resolve(&named);
    if resolved == named {
        vec![named]
    } else {
        vec![named, resolved]
    }
}

/// Where `path` leads: through every link, for as much of it as exists, and
/// as named below that. A link to a file that does not exist yet leads to
/// where writing through it would create one.
pub(super) fn resolve(path: &Path) -> PathBuf {
    resolve_through(path, 8)
}

fn resolve_through(path: &Path, links: u8) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    if links > 0 {
        if let Ok(target) = std::fs::read_link(path) {
            let parent = path.parent().unwrap_or(Path::new(""));
            return resolve_through(&normalize(&parent.join(target)), links - 1);
        }
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => resolve_through(parent, links).join(name),
        _ => path.to_path_buf(),
    }
}

/// Why `relative` (to a flow's repository, or its directory outside one)
/// decides whether the evidence is checked at all: the agent's settings,
/// which run the hook that enforces the guard, wherever in the repository
/// Claude Code is started; CI workflows, which run the flow; and git hooks
/// and configuration, which run code on their own.
pub(super) fn harness(relative: &str) -> Option<String> {
    let relative = folded(relative);
    let parts: Vec<&str> = relative.split('/').collect();
    let reason = match parts.as_slice() {
        [.., ".claude", name] if is_settings(name) => {
            "it configures Claude Code, including the hook that enforces this guard"
        }
        [".git", "config"] => "it is git configuration, which can point git at other hooks",
        [".git", "hooks"] => "it holds git hooks, code git runs on its own",
        [".git", "hooks", ..] => "it is a git hook, code git runs on its own",
        [".github", "workflows"] => "it holds CI workflows, which decide what CI checks",
        [".github", "workflows", ..] => "it is a CI workflow, which decides what CI checks",
        _ => return None,
    };
    Some(reason.to_string())
}

/// Why `relative` (to the user's home directory) decides whether the guard
/// runs at all.
pub(super) fn user_harness(relative: &str) -> Option<String> {
    let relative = folded(relative);
    let parts: Vec<&str> = relative.split('/').collect();
    let reason = match parts.as_slice() {
        [".claude", name] if is_settings(name) => {
            "it configures Claude Code for this user, including the hook that enforces this guard"
        }
        [".claude", "plugins"] => {
            "it holds this user's Claude Code plugins, one of which installs the hook that enforces this guard"
        }
        [".claude", "plugins", ..] => {
            "it is a Claude Code plugin for this user, or part of one, which can install or remove the hook that enforces this guard"
        }
        _ => return None,
    };
    Some(reason.to_string())
}

/// `settings.json`, `settings.local.json` and the like, which Claude Code
/// reads.
fn is_settings(name: &str) -> bool {
    name.starts_with("settings") && name.ends_with(".json")
}

/// How the guard matches a proof pattern: as a flow does, ignoring case
/// where the file system does.
pub(super) fn proof_match() -> glob::MatchOptions {
    glob::MatchOptions {
        case_sensitive: !FOLDS_CASE,
        ..crate::engine::keying::match_options()
    }
}

/// What `pattern` matches below `dir` on disk; a pattern without wildcards
/// is the one path it names.
pub(super) fn on_disk(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    if !pattern.contains(['*', '?', '[']) {
        return vec![dir.join(pattern)];
    }
    let pattern = format!(
        "{}/{pattern}",
        glob::Pattern::escape(&dir.to_string_lossy())
    );
    glob::glob_with(&pattern, proof_match())
        .map(|paths| paths.flatten().take(GUARD_WALK_LIMIT).collect())
        .unwrap_or_default()
}

/// The files below `dir`, without following links, up to `limit` entries.
#[cfg(unix)]
pub(super) fn files_below(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut queue = std::collections::VecDeque::from([dir.to_path_buf()]);
    let mut read = 0;
    while let Some(next) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            read += 1;
            if read > limit {
                return files;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => queue.push_back(entry.path()),
                Ok(kind) if kind.is_file() => files.push(entry.path()),
                _ => {}
            }
        }
    }
    files
}

/// `path` without `.` and with `..` applied, without touching the disk: the
/// file may not exist yet.
pub(super) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_named_below_a_base_in_any_case_where_names_fold() {
        let base = Path::new("/r/Proj");
        assert_eq!(
            strip_base(Path::new("/r/Proj/a/b"), base, false).as_deref(),
            Some("a/b")
        );
        assert_eq!(
            strip_base(Path::new("/r/Proj"), base, false).as_deref(),
            Some("")
        );
        assert_eq!(strip_base(Path::new("/r/proj/a"), base, false), None);
        assert_eq!(
            strip_base(Path::new("/r/proj/A"), base, true).as_deref(),
            Some("A")
        );
        assert_eq!(strip_base(Path::new("/r/Projects/a"), base, true), None);
    }
}
