//! Step identity: which facts decide whether a step has to run again.
//!
//! A step key is a digest over the command, the declared environment, the
//! proof command, the content of every declared input file and of every file
//! the proof reads, and the receipt ids of upstream steps. Equal keys mean the same work over the same facts.
//! Any change to what goes into the key must bump `SCHEMA_VERSION`, because it
//! changes every identity flot has ever recorded.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::core::digest;
use crate::core::flow::{Step, SCHEMA_VERSION};
use crate::core::receipt::{DepRef, FileDigest, ToolIdentity};

/// What a step reads, digested before it runs.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub inputs: Vec<FileDigest>,
    pub verify_inputs: Vec<FileDigest>,
    pub limitations: Vec<String>,
}

/// Digest everything a step declares it reads, relative to `root`.
pub fn facts_of(root: &Path, step: &Step) -> Facts {
    facts_with(root, step, &[])
}

/// [`facts_of`] a copy of the project at `root`, such as a candidate's,
/// read as `root` reads it: with the files `root`'s ignore rules let a
/// wildcard reach, and the executable bits `root`'s repository records. The
/// copy has no history of its own, and its key must be the one `root`
/// computes.
pub fn facts_of_copy(copy: &Path, root: &Path, step: &Step) -> Facts {
    facts_in(copy, Some(root), step, &[])
}

/// Whether git records each file it tracks under `root` as executable, when
/// git does not trust the file system's executable bit (`core.fileMode` is
/// false: on Windows, and on WSL's `/mnt` drives, where every file looks
/// executable). There the recorded bit is the one a fresh checkout on Linux
/// or macOS has, so a key computed here matches one computed there. `None`
/// where the file system's bit stands, or where git cannot say.
pub fn recorded_modes(root: &Path) -> Option<BTreeMap<String, bool>> {
    let git = crate::engine::git::Git::open(root).ok()?;
    if git.run(&["config", "--bool", "core.fileMode"], None).ok()? != "false" {
        return None;
    }
    let listed = git.output(&["ls-files", "-s", "-z"], None, false).ok()?;
    Some(
        listed
            .split(|&byte| byte == 0)
            .filter_map(|entry| {
                let entry = String::from_utf8_lossy(entry);
                let (meta, path) = entry.split_once('\t')?;
                Some((path.to_string(), meta.starts_with("100755 ")))
            })
            .collect(),
    )
}

/// Like `facts_of`, as the files will be once `pending` are in place: files
/// that are missing now but that reusing an upstream receipt will restore.
/// `plan` needs this to predict what `run` will see.
pub fn facts_with(root: &Path, step: &Step, pending: &[FileDigest]) -> Facts {
    facts_in(root, None, step, pending)
}

/// The facts of `step` in `root`, a copy of `origin` when that is given.
fn facts_in(root: &Path, origin: Option<&Path>, step: &Step, pending: &[FileDigest]) -> Facts {
    let (inputs, mut limitations) = digest_in(root, origin, &step.inputs, "input", pending);
    let (verify_inputs, proof_limitations) =
        digest_in(root, origin, &step.verify_inputs, "verify input", pending);
    limitations.extend(proof_limitations);
    // A step that always runs is never reused, whatever its key.
    if step.inputs.is_empty() && !step.always {
        limitations.push(
            "step declares no inputs; only its command and dependencies key it, so it is reused even when files it reads change"
                .into(),
        );
    }
    Facts {
        inputs,
        verify_inputs,
        limitations,
    }
}

/// Paths whose content differs between two digests of the same globs: changed,
/// created or deleted.
pub fn changed_paths(before: &[FileDigest], after: &[FileDigest]) -> Vec<String> {
    let before: BTreeMap<&str, (&str, bool)> = before
        .iter()
        .map(|input| {
            (
                input.path.as_str(),
                (input.digest.as_str(), input.executable),
            )
        })
        .collect();
    let after: BTreeMap<&str, (&str, bool)> = after
        .iter()
        .map(|input| {
            (
                input.path.as_str(),
                (input.digest.as_str(), input.executable),
            )
        })
        .collect();
    let paths: std::collections::BTreeSet<&str> =
        before.keys().chain(after.keys()).copied().collect();
    paths
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .map(str::to_string)
        .collect()
}

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

/// The ignore rules of a project, applied to a copy of it.
///
/// A copy lives in the store, whose `.gitignore` ignores everything, and
/// what lies above the store is the project and its parents, not the copy's:
/// read from there, the rules would hide every file of the copy. Here they
/// are what a scan of the project itself reads, in the same order: the
/// copy's own `.gitignore` files, those of the directories above the project,
/// `info/exclude` of every repository at or above each of them, and the
/// user's global excludes. The first one that matches decides.
struct CopyIgnore {
    copy: PathBuf,
    /// The project, as its absolute path.
    origin: PathBuf,
    /// `.gitignore` of each directory above the project, nearest first.
    above: Vec<Gitignore>,
    /// `info/exclude` of each repository above the project, nearest first.
    excludes: Vec<Gitignore>,
    global: Gitignore,
    /// The rules of each directory of the copy, read once.
    levels: Mutex<HashMap<PathBuf, Arc<Level>>>,
}

/// The rules one directory of a copy contributes.
struct Level {
    /// Its `.gitignore`, as the copy holds it.
    gitignore: Gitignore,
    /// `info/exclude` of a repository at the same place in the project: a
    /// copy has no `.git`.
    exclude: Option<Gitignore>,
}

impl CopyIgnore {
    fn new(copy: &Path, origin: &Path) -> Self {
        let origin = origin
            .canonicalize()
            .unwrap_or_else(|_| origin.to_path_buf());
        let parents: Vec<&Path> = origin.ancestors().skip(1).collect();
        CopyIgnore {
            copy: copy.to_path_buf(),
            above: parents
                .iter()
                .map(|dir| Gitignore::new(dir.join(".gitignore")).0)
                .collect(),
            excludes: parents.iter().filter_map(|dir| exclude_of(dir)).collect(),
            global: Gitignore::global().0,
            levels: Mutex::new(HashMap::new()),
            origin,
        }
    }

    fn level(&self, dir: &Path) -> Arc<Level> {
        let mut levels = self
            .levels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        levels
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                let relative = dir.strip_prefix(&self.copy).unwrap_or(Path::new(""));
                Arc::new(Level {
                    gitignore: Gitignore::new(dir.join(".gitignore")).0,
                    exclude: exclude_of(&self.origin.join(relative)),
                })
            })
            .clone()
    }

    /// Whether the project's rules ignore `path`, a path in the copy.
    fn ignores(&self, path: &Path, is_dir: bool) -> bool {
        let Ok(relative) = path.strip_prefix(&self.copy) else {
            return false;
        };
        let mapped = self.origin.join(relative);
        let levels: Vec<Arc<Level>> = path
            .ancestors()
            .skip(1)
            .take_while(|dir| dir.starts_with(&self.copy))
            .map(|dir| self.level(dir))
            .collect();
        let decided = levels
            .iter()
            .map(|level| level.gitignore.matched(path, is_dir))
            .chain(
                self.above
                    .iter()
                    .map(|rules| rules.matched(&mapped, is_dir)),
            )
            .chain(
                levels
                    .iter()
                    .filter_map(|level| level.exclude.as_ref())
                    .map(|rules| rules.matched(&mapped, is_dir)),
            )
            .chain(
                self.excludes
                    .iter()
                    .map(|rules| rules.matched(&mapped, is_dir)),
            )
            .chain(std::iter::once(self.global.matched(&mapped, is_dir)))
            .find(|matched| !matched.is_none());
        decided.is_some_and(|matched| matched.is_ignore())
    }
}

/// `info/exclude` of the repository whose work tree is `dir`, found as a
/// scan of the project finds it: in `.git`, or for a linked work tree in the
/// common directory its `.git` file leads to.
fn exclude_of(dir: &Path) -> Option<Gitignore> {
    let dot_git = dir.join(".git");
    let meta = std::fs::metadata(&dot_git).ok()?;
    let git_dir = if meta.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let real = PathBuf::from(text.lines().next()?.strip_prefix("gitdir: ")?);
        let common = std::fs::read_to_string(real.join("commondir")).ok()?;
        let common = common.lines().next()?;
        if common.starts_with('.') {
            real.join(common)
        } else {
            PathBuf::from(common)
        }
    } else {
        dot_git
    };
    let mut builder = GitignoreBuilder::new(dir);
    let _ = builder.add(git_dir.join("info").join("exclude"));
    builder.build().ok()
}

/// Files under `root` a pattern names. A wildcard never enters `.git` or the
/// store, and with [`Reach::Tracked`] it skips what git ignores below the
/// pattern's literal directories; a path the pattern spells
/// out is always followed. In a copy of the project, what git ignores is
/// what it ignores in the project (`copy`).
fn candidates(
    root: &Path,
    pattern: &str,
    reach: Reach,
    copy: Option<&Arc<CopyIgnore>>,
) -> Result<Vec<PathBuf>, String> {
    let compiled = glob::Pattern::new(pattern).map_err(|error| error.to_string())?;
    let (base, wild) = literal_base(pattern);
    if !wild {
        let path = root.join(pattern);
        return Ok(if path.is_file() {
            vec![path]
        } else {
            Vec::new()
        });
    }
    let start = root.join(base);
    if !start.is_dir() {
        return Ok(Vec::new());
    }
    // A copy's rules are the project's, applied below; the walker would read
    // the store's.
    let copy = copy.filter(|_| reach == Reach::Tracked).cloned();
    let tracked = reach == Reach::Tracked && copy.is_none();
    let walker = ignore::WalkBuilder::new(&start)
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
    Ok(walker
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|path| {
            path.strip_prefix(root)
                .is_ok_and(|relative| compiled.matches_path_with(relative, match_options()))
        })
        .collect())
}

/// Resolve globs, relative to `root`, handing each matched file's bytes to
/// `keep`, which returns the digest to record for it. Files are read one at a
/// time so a large tree is never held in memory at once.
pub fn scan_globs<E>(
    root: &Path,
    patterns: &[String],
    kind: &str,
    reach: Reach,
    keep: impl FnMut(&[u8]) -> Result<String, E>,
) -> Result<Scan, E> {
    scan_in(root, None, patterns, kind, reach, true, keep)
}

/// [`scan_globs`] in `root`, a copy of `origin` when that is given, whose
/// ignore rules and recorded executable bits then apply. Without `modes`,
/// every file keeps the bit the file system shows.
fn scan_in<E>(
    root: &Path,
    origin: Option<&Path>,
    patterns: &[String],
    kind: &str,
    reach: Reach,
    modes: bool,
    mut keep: impl FnMut(&[u8]) -> Result<String, E>,
) -> Result<Scan, E> {
    let mut files: BTreeMap<String, (String, bool)> = BTreeMap::new();
    let mut scan = Scan::default();
    // What a step reads is keyed as git records it where git does not trust
    // the file system; what it produces keeps the bit it was written with,
    // which reuse restores.
    let recorded = match reach {
        Reach::Tracked if modes => recorded_modes(origin.unwrap_or(root)),
        _ => None,
    };
    let copy = origin
        .filter(|_| reach == Reach::Tracked)
        .map(|origin| Arc::new(CopyIgnore::new(root, origin)));
    for pattern in patterns {
        let entries = match candidates(root, pattern, reach, copy.as_ref()) {
            Ok(entries) => entries,
            Err(error) => {
                scan.problems
                    .push(format!("{kind} pattern `{pattern}` is invalid: {error}"));
                continue;
            }
        };
        let mut matched = 0usize;
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
            if files.contains_key(&relative) {
                matched += 1;
                continue;
            }
            match std::fs::read(&entry) {
                Ok(bytes) => {
                    let executable = recorded
                        .as_ref()
                        .and_then(|modes| modes.get(&relative).copied())
                        .unwrap_or_else(|| is_executable(&entry));
                    files.insert(relative, (keep(&bytes)?, executable));
                    matched += 1;
                }
                Err(error) => scan
                    .problems
                    .push(format!("{kind} `{relative}` could not be read: {error}")),
            }
        }
        if matched == 0 {
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

/// Copy the project into `dest` for one candidate of `step`: every file a
/// wildcard would reach (so not what git ignores, and never `.git` or the
/// store), and every file the step names as an input, proof input or output,
/// ignored or not. Files are copied with their modes; links are followed.
pub fn copy_workspace(root: &Path, dest: &Path, step: &Step) -> Result<()> {
    let mut files: std::collections::BTreeSet<PathBuf> =
        candidates(root, "**/*", Reach::Tracked, None)
            .map_err(|error| anyhow::anyhow!("cannot list the project's files: {error}"))?
            .into_iter()
            .collect();
    for (patterns, reach) in [
        (&step.inputs, Reach::Tracked),
        (&step.verify_inputs, Reach::Tracked),
        (&step.outputs, Reach::Everything),
    ] {
        for pattern in patterns {
            // An invalid pattern is reported where the step's facts are read.
            if let Ok(found) = candidates(root, pattern, reach, None) {
                files.extend(found);
            }
        }
    }
    std::fs::create_dir_all(dest).with_context(|| format!("cannot create {}", dest.display()))?;
    for path in files {
        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("{} is outside the project", path.display()))?;
        let target = dest.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        std::fs::copy(&path, &target)
            .with_context(|| format!("cannot copy {}", relative.display()))?;
    }
    Ok(())
}

/// Whether anyone may execute the file.
pub fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
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

/// Digest the files a step reads. Patterns that match nothing are reported,
/// because a key that cannot see a file cannot notice it changing.
pub fn digest_globs(
    root: &Path,
    patterns: &[String],
    kind: &str,
    pending: &[FileDigest],
) -> (Vec<FileDigest>, Vec<String>) {
    digest_in(root, None, patterns, kind, pending)
}

/// [`digest_globs`] in `root`, a copy of `origin` when that is given.
fn digest_in(
    root: &Path,
    origin: Option<&Path>,
    patterns: &[String],
    kind: &str,
    pending: &[FileDigest],
) -> (Vec<FileDigest>, Vec<String>) {
    let mut scan = match scan_in(
        root,
        origin,
        patterns,
        kind,
        Reach::Tracked,
        true,
        |bytes| Ok::<_, std::convert::Infallible>(digest::bytes(bytes)),
    ) {
        Ok(scan) => scan,
        Err(never) => match never {},
    };
    for file in pending {
        if scan.files.iter().any(|found| found.path == file.path) {
            continue;
        }
        let matching: Vec<String> = patterns
            .iter()
            .filter(|pattern| {
                glob::Pattern::new(pattern)
                    .is_ok_and(|compiled| compiled.matches_with(&file.path, match_options()))
            })
            .cloned()
            .collect();
        if !matching.is_empty() {
            scan.files.push(file.clone());
            scan.unmatched.retain(|pattern| !matching.contains(pattern));
        }
    }
    scan.files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut limitations = scan.problems;
    limitations.extend(scan.unmatched.iter().map(|pattern| {
        format!(
            "{kind} pattern `{pattern}` matched no readable file; this step's cache key cannot see those changes"
        )
    }));
    (scan.files, limitations)
}

pub fn step_key(flow_name: &str, step: &Step, facts: &Facts, deps: &[DepRef]) -> String {
    let mut key = serde_json::json!({
        "schema": SCHEMA_VERSION,
        "flow": flow_name,
        "step_id": step.id,
        "command": step.run,
        "verify": step.verify,
        "claims": step.claims,
        "env": step.env,
        "allow_failure": step.allow_failure,
        "inputs": facts.inputs,
        "verify_inputs": facts.verify_inputs,
        "outputs": step.outputs,
        "deps": deps,
        "tool": ToolIdentity::default(),
    });
    // Only when set, so steps without it keep the keys they had.
    if !step.may_change.is_empty() {
        key["may_change"] = serde_json::json!(step.may_change);
    }
    if let Some(sandbox) = &step.sandbox {
        key["sandbox"] = serde_json::json!(sandbox);
    }
    if step.fails_before {
        key["fails_before"] = serde_json::json!(true);
    }
    digest::canonical(&key)
}

/// Every file a wildcard reaches under `root` (so not what git ignores, and
/// never `.git` or the store), with the digest of its content. `root` is the
/// project `origin` or a copy of it, read with the project's ignore rules.
pub fn snapshot(root: &Path, origin: &Path) -> Scan {
    scan_in(
        root,
        (root != origin).then_some(origin),
        &["**/*".to_string()],
        "project file",
        Reach::Tracked,
        false,
        |bytes| Ok::<_, std::convert::Infallible>(digest::bytes(bytes)),
    )
    .unwrap_or_else(|never| match never {})
}

/// Files whose content differs between two snapshots: created or edited,
/// with their content afterwards, or deleted, as `None`. A change of mode
/// alone is not a change of content.
pub fn changes(before: &[FileDigest], after: &[FileDigest]) -> BTreeMap<String, Option<String>> {
    let before: BTreeMap<&str, &str> = before
        .iter()
        .map(|file| (file.path.as_str(), file.digest.as_str()))
        .collect();
    let after: BTreeMap<&str, &str> = after
        .iter()
        .map(|file| (file.path.as_str(), file.digest.as_str()))
        .collect();
    before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .map(|path| {
            (
                path.to_string(),
                after.get(path).map(|digest| digest.to_string()),
            )
        })
        .collect()
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

/// Files created, edited or deleted between two snapshots that `step` may not
/// change: not matched by its `may_change` or its `outputs`.
pub fn out_of_scope(step: &Step, before: &[FileDigest], after: &[FileDigest]) -> Vec<String> {
    let allowed: Vec<glob::Pattern> = step
        .may_change
        .iter()
        .chain(&step.outputs)
        .filter_map(|pattern| glob::Pattern::new(pattern).ok())
        .collect();
    let before: BTreeMap<&str, &str> = before
        .iter()
        .map(|file| (file.path.as_str(), file.digest.as_str()))
        .collect();
    let after: BTreeMap<&str, &str> = after
        .iter()
        .map(|file| (file.path.as_str(), file.digest.as_str()))
        .collect();
    let mut changed: Vec<String> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .filter(|path| {
            !allowed
                .iter()
                .any(|pattern| pattern.matches_with(path, match_options()))
        })
        .map(|path| path.to_string())
        .collect();
    changed.sort();
    changed.dedup();
    changed
}

/// Upstream receipt references in need-name order, or `None` when a dependency
/// has no receipt yet and the key is therefore not knowable.
pub fn dep_refs(step: &Step, receipts: &HashMap<String, String>) -> Option<Vec<DepRef>> {
    let mut needs = step.needs.clone();
    needs.sort();
    needs
        .into_iter()
        .map(|need| {
            receipts.get(&need).map(|receipt_id| DepRef {
                receipt_id: receipt_id.clone(),
                step_id: need,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::flow::Flow;

    fn step(extra: &str) -> Step {
        let text = format!("[flow]\nname = \"k\"\n\n[[step]]\nid = \"s\"\nrun = \"echo s\"\n{extra}\n\n[[step]]\nid = \"a\"\nrun = \"true\"\n\n[[step]]\nid = \"b\"\nrun = \"true\"\n");
        Flow::parse(&text).unwrap().step("s").unwrap().clone()
    }

    fn reads(inputs: &[FileDigest], verify_inputs: &[FileDigest]) -> Facts {
        Facts {
            inputs: inputs.to_vec(),
            verify_inputs: verify_inputs.to_vec(),
            limitations: Vec::new(),
        }
    }

    fn input(path: &str, content: &[u8]) -> FileDigest {
        FileDigest {
            executable: false,
            path: path.into(),
            digest: digest::bytes(content),
        }
    }

    #[test]
    fn the_key_tracks_input_content() {
        let step = step("");
        assert_ne!(
            step_key("k", &step, &reads(&[input("x", b"1")], &[]), &[]),
            step_key("k", &step, &reads(&[input("x", b"2")], &[]), &[])
        );
    }

    #[test]
    fn the_key_tracks_the_proof_command() {
        assert_ne!(
            step_key("k", &step("verify = \"true\""), &Facts::default(), &[]),
            step_key("k", &step("verify = \"false\""), &Facts::default(), &[])
        );
    }

    #[test]
    fn the_key_tracks_upstream_receipts() {
        let step = step("");
        let dep = |receipt: &str| DepRef {
            step_id: "a".into(),
            receipt_id: receipt.into(),
        };
        assert_ne!(
            step_key("k", &step, &Facts::default(), &[dep("sha256:1")]),
            step_key("k", &step, &Facts::default(), &[dep("sha256:2")])
        );
    }

    #[test]
    fn dependency_references_ignore_declaration_order() {
        let receipts = HashMap::from([
            ("a".to_string(), "sha256:a".to_string()),
            ("b".to_string(), "sha256:b".to_string()),
        ]);
        let forward = dep_refs(&step("needs = [\"a\", \"b\"]"), &receipts).unwrap();
        let backward = dep_refs(&step("needs = [\"b\", \"a\"]"), &receipts).unwrap();
        assert_eq!(forward, backward);
    }

    #[test]
    fn an_unsettled_dependency_makes_the_key_unknowable() {
        let receipts = HashMap::from([("a".to_string(), "sha256:a".to_string())]);
        assert!(dep_refs(&step("needs = [\"a\", \"b\"]"), &receipts).is_none());
    }

    #[test]
    fn inputs_are_sorted_and_unmatched_patterns_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let facts = facts_of(dir.path(), &step("inputs = [\"*.txt\", \"missing/**\"]"));
        let (inputs, limitations) = (facts.inputs, facts.limitations);
        let paths: Vec<&str> = inputs.iter().map(|input| input.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "b.txt"]);
        assert_eq!(limitations.len(), 1);
        assert!(limitations[0].contains("missing/**"), "{}", limitations[0]);
    }

    #[test]
    fn the_key_tracks_the_content_of_the_proof() {
        let step = step("verify = \"python3 check.py\"");
        assert_ne!(
            step_key("k", &step, &reads(&[], &[input("check.py", b"v1")]), &[]),
            step_key("k", &step, &reads(&[], &[input("check.py", b"v2")]), &[])
        );
    }

    #[test]
    fn proof_files_and_work_files_are_not_interchangeable() {
        let step = step("verify = \"true\"");
        let file = input("x", b"1");
        assert_ne!(
            step_key("k", &step, &reads(std::slice::from_ref(&file), &[]), &[]),
            step_key("k", &step, &reads(&[], std::slice::from_ref(&file)), &[])
        );
    }

    /// The exact bytes a step key digests. Every receipt id, cache entry and
    /// run root follows from them, so any change here must bump
    /// `SCHEMA_VERSION`. Only flot's own version is filled in, so a release
    /// leaves this test as it is.
    #[test]
    fn the_key_digests_exactly_these_bytes() {
        let golden = |text: &str| {
            digest::bytes(
                text.replace("@VERSION@", env!("CARGO_PKG_VERSION"))
                    .as_bytes(),
            )
        };
        let facts = Facts {
            inputs: vec![
                FileDigest {
                    executable: true,
                    path: "run.sh".into(),
                    digest: digest::bytes(b"make\n"),
                },
                input("src/a.txt", b"a"),
            ],
            verify_inputs: vec![input("check.sh", b"grep -q 1 out.txt\n")],
            limitations: vec!["not part of the key".into()],
        };
        let deps = [DepRef {
            step_id: "a".into(),
            receipt_id: digest::bytes(b"receipt of a"),
        }];

        let plain = step("");
        assert_eq!(
            step_key("k", &plain, &Facts::default(), &[]),
            golden(PLAIN_KEY),
            "a step that sets nothing optional"
        );

        let full = step(
            "verify = \"sh check.sh\"\nclaims = [\"out.txt holds 1\"]\nenv = { LANG = \"C\", A = \"1\" }\nallow_failure = true\noutputs = [\"out.txt\"]\ninputs = [\"src/*.txt\", \"run.sh\"]\nneeds = [\"a\"]\nmay_change = [\"notes/*\"]\nsandbox = { network = true, write = [\"out\"] }\nfails_before = true\nattempts = 3\nbudget = { timeout_secs = 5 }",
        );
        assert_eq!(
            step_key("k", &full, &facts, &deps),
            golden(FULL_KEY),
            "a step that sets every field the key reads"
        );
    }

    const PLAIN_KEY: &str = r#"{"allow_failure":false,"claims":[],"command":"echo s","deps":[],"env":{},"flow":"k","inputs":[],"outputs":[],"schema":5,"step_id":"s","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify":null,"verify_inputs":[]}"#;

    const FULL_KEY: &str = r#"{"allow_failure":true,"claims":["out.txt holds 1"],"command":"echo s","deps":[{"receipt_id":"sha256:0eba9c324da1c0ec8fc47af90ba25ab329eeb9744993eac814bdd8da10002cee","step_id":"a"}],"env":{"A":"1","LANG":"C"},"fails_before":true,"flow":"k","inputs":[{"digest":"sha256:c117ade9ea8a00abe926dfed79297a9a3632defb97a3ffd1baf33fe92704aa0f","executable":true,"path":"run.sh"},{"digest":"sha256:ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb","path":"src/a.txt"}],"may_change":["notes/*"],"outputs":["out.txt"],"sandbox":{"network":true,"write":["out"]},"schema":5,"step_id":"s","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify":"sh check.sh","verify_inputs":[{"digest":"sha256:a8b1f082c2590c4c3855b71427328ecab1ebb40701108a07be7b70f9c926f225","path":"check.sh"}]}"#;

    #[test]
    fn changed_paths_reports_edits_creations_and_deletions() {
        let before = [
            input("kept", b"1"),
            input("edited", b"1"),
            input("gone", b"1"),
        ];
        let after = [
            input("kept", b"1"),
            input("edited", b"2"),
            input("new", b"1"),
        ];
        assert_eq!(
            changed_paths(&before, &after),
            vec!["edited", "gone", "new"]
        );
        assert!(changed_paths(&before, &before).is_empty());
    }

    fn matched(root: &Path, pattern: &str, reach: Reach) -> Vec<String> {
        let scan = scan_globs(root, &[pattern.to_string()], "input", reach, |_| {
            Ok::<_, std::convert::Infallible>(String::new())
        })
        .unwrap_or_else(|never| match never {});
        scan.files.into_iter().map(|file| file.path).collect()
    }

    #[test]
    fn a_literal_base_stops_at_the_first_wildcard() {
        assert_eq!(literal_base("Cargo.toml"), ("", false));
        assert_eq!(literal_base("src/lib.rs"), ("src", false));
        assert_eq!(literal_base("src/**/*.rs"), ("src", true));
        assert_eq!(literal_base("a/b/c*/d"), ("a/b", true));
        assert_eq!(literal_base("**/*"), ("", true));
        assert_eq!(literal_base("src/[ab].rs"), ("src", true));
    }

    #[test]
    fn a_wildcard_input_skips_what_git_ignores_unless_the_pattern_spells_it_out() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for path in [
            "src/lib.rs",
            "target/debug/gen.rs",
            ".git/hooks/x.rs",
            ".flot/tmp/y.rs",
        ] {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), "fn main() {}").unwrap();
        }
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();

        assert_eq!(matched(root, "**/*.rs", Reach::Tracked), ["src/lib.rs"]);
        // Named through its literal directories, an ignored tree is an input.
        assert_eq!(
            matched(root, "target/**/*.rs", Reach::Tracked),
            ["target/debug/gen.rs"]
        );
        assert_eq!(
            matched(root, "target/debug/gen.rs", Reach::Tracked),
            ["target/debug/gen.rs"]
        );
        // Products are often ignored build output; a wildcard reaches them,
        // but never git's own files or the store.
        assert_eq!(
            matched(root, "**/*.rs", Reach::Everything),
            ["src/lib.rs", "target/debug/gen.rs"]
        );
    }
}
