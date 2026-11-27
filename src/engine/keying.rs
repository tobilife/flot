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

mod digests;
pub use digests::Digests;

/// What a step reads, digested before it runs.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub inputs: Vec<FileDigest>,
    pub verify_inputs: Vec<FileDigest>,
    pub limitations: Vec<String>,
}

/// Digest everything a step declares it reads, relative to `root`.
pub fn facts_of(root: &Path, step: &Step, digests: &Digests) -> Facts {
    facts_with(root, step, &[], digests)
}

/// [`facts_of`] a copy of the project at `root`, such as a candidate's,
/// read as `root` reads it: with the files `root`'s ignore rules let a
/// wildcard reach, and the executable bits `root`'s repository records. The
/// copy has no history of its own, and its key must be the one `root`
/// computes.
pub fn facts_of_copy(copy: &Path, root: &Path, step: &Step, digests: &Digests) -> Facts {
    facts_in(copy, Some(root), step, &[], digests)
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
pub fn facts_with(root: &Path, step: &Step, pending: &[FileDigest], digests: &Digests) -> Facts {
    facts_in(root, None, step, pending, digests)
}

/// The facts of `step` in `root`, a copy of `origin` when that is given.
fn facts_in(
    root: &Path,
    origin: Option<&Path>,
    step: &Step,
    pending: &[FileDigest],
    digests: &Digests,
) -> Facts {
    let (inputs, mut limitations) =
        digest_in(root, origin, &step.inputs, "input", pending, digests);
    let (verify_inputs, proof_limitations) = digest_in(
        root,
        origin,
        &step.verify_inputs,
        "verify input",
        pending,
        digests,
    );
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
    candidates_of(root, &[pattern], reach, copy)
        .pop()
        .expect("one pattern has one answer")
}

/// [`candidates`] for each of `patterns`, in order. Patterns whose wildcards
/// start in the same directory share one walk of it: on a network or 9p
/// mount a walk is most of a scan, and `**/*.ts`, `**/*.tsx` and `**/*.json`
/// would otherwise walk the whole project three times.
fn candidates_of(
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
enum Keep<'a, E> {
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
fn scan_in<E>(
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
fn digest_all(
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

/// Copy the project into `dest` for one candidate of `step`: the files of
/// [`workspace_files`], with their modes; links are followed.
pub fn copy_workspace(root: &Path, dest: &Path, step: &Step) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("cannot create {}", dest.display()))?;
    for relative in workspace_files(root, step)? {
        let target = dest.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        copy_new(&root.join(&relative), &target, None)
            .with_context(|| format!("cannot copy {}", relative.display()))?;
    }
    Ok(())
}

/// Bring `dest`, a copy [`sync_workspace`] made for an earlier proof, to
/// what [`copy_workspace`] would make of the work now. A file the work holds
/// otherwise is written again, its modification time `stamp`, and anything
/// else in the copy (a link included) is removed; the rest is left as it is,
/// so a build that goes by times rebuilds only what changed. `work` and
/// `copied` know what the work's and the copy's files held when last read.
/// Returns how many files were written.
pub fn sync_workspace(
    root: &Path,
    dest: &Path,
    step: &Step,
    work: &Digests,
    copied: &Digests,
    stamp: std::time::SystemTime,
) -> Result<usize> {
    let files = workspace_files(root, step)?;
    let mut dirs: std::collections::BTreeSet<&Path> = std::collections::BTreeSet::new();
    for file in &files {
        dirs.extend(
            file.ancestors()
                .skip(1)
                .filter(|dir| !dir.as_os_str().is_empty()),
        );
    }
    std::fs::create_dir_all(dest).with_context(|| format!("cannot create {}", dest.display()))?;
    prune_copy(dest, Path::new(""), &files, &dirs)?;

    let pairs = |base: &Path| -> Vec<(String, PathBuf)> {
        files
            .iter()
            .map(|file| (String::new(), base.join(file)))
            .collect()
    };
    let wanted = digest_all(&pairs(root), work);
    let present = digest_all(&pairs(dest), copied);
    let mut written = 0;
    for ((relative, wanted), present) in files.iter().zip(wanted).zip(present) {
        let wanted = wanted.with_context(|| format!("cannot read {}", relative.display()))?;
        if present.is_ok_and(|present| present == wanted) {
            continue;
        }
        let target = dest.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        if target.symlink_metadata().is_ok() {
            std::fs::remove_file(&target)
                .with_context(|| format!("cannot replace {}", relative.display()))?;
        }
        copy_new(&root.join(relative), &target, Some(stamp))
            .with_context(|| format!("cannot copy {}", relative.display()))?;
        written += 1;
    }
    Ok(written)
}

/// Remove from the copy's directory `dest/at` what the work does not hold
/// there: links, files not in `files`, directories not in `dirs`.
fn prune_copy(
    dest: &Path,
    at: &Path,
    files: &std::collections::BTreeSet<PathBuf>,
    dirs: &std::collections::BTreeSet<&Path>,
) -> Result<()> {
    let dir = dest.join(at);
    let entries =
        std::fs::read_dir(&dir).with_context(|| format!("cannot list {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot list {}", dir.display()))?;
        let relative = at.join(entry.file_name());
        let kind = entry
            .file_type()
            .with_context(|| format!("cannot read {}", entry.path().display()))?;
        let removed = if kind.is_dir() {
            if dirs.contains(relative.as_path()) {
                prune_copy(dest, &relative, files, dirs)?;
                continue;
            }
            std::fs::remove_dir_all(entry.path())
        } else if kind.is_file() && files.contains(&relative) {
            continue;
        } else {
            std::fs::remove_file(entry.path())
        };
        removed.with_context(|| format!("cannot remove {} from the copy", relative.display()))?;
    }
    Ok(())
}

/// The files a copy of the work for `step` holds, relative to `root`: every
/// file a wildcard would reach (so not what git ignores, and never `.git` or
/// the store), and every file the step names as an input, proof input or
/// output, ignored or not.
fn workspace_files(root: &Path, step: &Step) -> Result<std::collections::BTreeSet<PathBuf>> {
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
    files
        .into_iter()
        .map(|path| {
            path.strip_prefix(root)
                .map(Path::to_path_buf)
                .with_context(|| format!("{} is outside the project", path.display()))
        })
        .collect()
}

/// Copy `from` to the new file `to`, with `from`'s permissions, modified
/// now or at `stamp`. Not `std::fs::copy`, which on macOS clones the file and
/// on Windows copies it whole, its modification time with it: a build that
/// goes by times would take a copied file older than its last build for one
/// it already built.
fn copy_new(from: &Path, to: &Path, stamp: Option<std::time::SystemTime>) -> std::io::Result<()> {
    let mut source = std::fs::File::open(from)?;
    let permissions = source.metadata()?.permissions();
    let mut target = std::fs::File::create(to)?;
    std::io::copy(&mut source, &mut target)?;
    if let Some(stamp) = stamp {
        target.set_modified(stamp)?;
    }
    target.set_permissions(permissions)
}

/// A file's bytes and what the file system says of it, both from one open
/// handle: asking by path again costs another round trip, which on a network
/// or 9p mount is as slow as the read. The metadata is taken before the
/// bytes, so a change while they are read is a change after it.
fn read_file(path: &Path) -> std::io::Result<(Vec<u8>, std::fs::Metadata)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    Ok((bytes, meta))
}

/// Whether anyone may execute the file `meta` describes.
fn executable(meta: &std::fs::Metadata) -> bool {
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

/// Digest the files a step reads, in `root`, a copy of `origin` when that is
/// given. Patterns that match nothing are reported, because a key that cannot
/// see a file cannot notice it changing.
fn digest_in(
    root: &Path,
    origin: Option<&Path>,
    patterns: &[String],
    kind: &str,
    pending: &[FileDigest],
    digests: &Digests,
) -> (Vec<FileDigest>, Vec<String>) {
    let mut scan = match scan_in(
        root,
        origin,
        patterns,
        kind,
        Reach::Tracked,
        true,
        Keep::<std::convert::Infallible>::Digest(digests),
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

/// What the files `patterns` match hold here, read as a proof input is.
pub fn held_files(root: &Path, patterns: &[String], digests: &Digests) -> Vec<FileDigest> {
    digest_in(root, None, patterns, "held file", &[], digests).0
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
    if !step.held.is_empty() {
        key["held"] = serde_json::json!(step.held);
    }
    digest::canonical(&key)
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
        let facts = facts_of(
            dir.path(),
            &step("inputs = [\"*.txt\", \"missing/**\"]"),
            &Digests::default(),
        );
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
            "verify = \"sh check.sh\"\nclaims = [\"out.txt holds 1\"]\nenv = { LANG = \"C\", A = \"1\" }\nallow_failure = true\noutputs = [\"out.txt\"]\ninputs = [\"src/*.txt\", \"run.sh\"]\nneeds = [\"a\"]\nmay_change = [\"notes/*\"]\nsandbox = { network = true, write = [\"out\"] }\nfails_before = true\nheld = [\"tests/*\"]\nattempts = 3\nbudget = { timeout_secs = 5 }",
        );
        assert_eq!(
            step_key("k", &full, &facts, &deps),
            golden(FULL_KEY),
            "a step that sets every field the key reads"
        );
    }

    const PLAIN_KEY: &str = r#"{"allow_failure":false,"claims":[],"command":"echo s","deps":[],"env":{},"flow":"k","inputs":[],"outputs":[],"schema":5,"step_id":"s","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify":null,"verify_inputs":[]}"#;

    const FULL_KEY: &str = r#"{"allow_failure":true,"claims":["out.txt holds 1"],"command":"echo s","deps":[{"receipt_id":"sha256:0eba9c324da1c0ec8fc47af90ba25ab329eeb9744993eac814bdd8da10002cee","step_id":"a"}],"env":{"A":"1","LANG":"C"},"fails_before":true,"flow":"k","held":["tests/*"],"inputs":[{"digest":"sha256:c117ade9ea8a00abe926dfed79297a9a3632defb97a3ffd1baf33fe92704aa0f","executable":true,"path":"run.sh"},{"digest":"sha256:ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb","path":"src/a.txt"}],"may_change":["notes/*"],"outputs":["out.txt"],"sandbox":{"network":true,"write":["out"]},"schema":5,"step_id":"s","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify":"sh check.sh","verify_inputs":[{"digest":"sha256:a8b1f082c2590c4c3855b71427328ecab1ebb40701108a07be7b70f9c926f225","path":"check.sh"}]}"#;

    /// Over this repository's own files, enough for several threads, the
    /// files read at once get the digests, bits and order that reading them
    /// one at a time gives. The bits are the file system's, as a snapshot
    /// reads them, not the ones git records.
    #[test]
    fn reading_files_at_once_records_what_reading_them_in_turn_does() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let patterns = ["src/**/*.rs".to_string(), "tests/**/*".to_string()];
        let scan = |keep| scan_in(root, None, &patterns, "input", Reach::Tracked, false, keep);
        let digests = Digests::default();
        let at_once = scan(Keep::<std::convert::Infallible>::Digest(&digests)).unwrap();
        let mut one = |bytes: &[u8]| Ok(digest::bytes(bytes));
        let in_turn = scan(Keep::With(&mut one)).unwrap();
        assert!(at_once.files.len() >= 64, "{} files", at_once.files.len());
        assert_eq!(at_once.files, in_turn.files);
        assert!(at_once.problems.is_empty() && at_once.unmatched.is_empty());
    }

    /// Patterns walked together in this repository find, each, what walking
    /// for it alone finds: shared starts, nested starts, a literal path, a
    /// pattern matching nothing and an invalid one.
    #[test]
    fn patterns_walked_together_find_what_each_finds_alone() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let patterns = [
            "**/*.rs",
            "**/*.toml",
            "src/**/*.rs",
            "src/engine/*.rs",
            "Cargo.toml",
            "**/*.nothing",
            "[",
        ];
        let together = candidates_of(root, &patterns, Reach::Tracked, None);
        assert_eq!(together.len(), patterns.len());
        for (pattern, found) in patterns.iter().zip(together) {
            assert_eq!(
                found,
                candidates(root, pattern, Reach::Tracked, None),
                "{pattern}"
            );
            if !["**/*.nothing", "["].contains(pattern) {
                assert!(!found.unwrap().is_empty(), "{pattern} found nothing");
            }
        }
    }

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

    #[test]
    fn a_copy_of_the_work_is_written_when_it_is_made() {
        let dir = tempfile::tempdir().unwrap();
        let (root, dest) = (dir.path().join("work"), dir.path().join("copy"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("old.rs"), "fn old() {}").unwrap();
        let long_ago =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        std::fs::File::options()
            .write(true)
            .open(root.join("old.rs"))
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        let mut readonly = std::fs::metadata(root.join("old.rs"))
            .unwrap()
            .permissions();
        readonly.set_readonly(true);
        std::fs::set_permissions(root.join("old.rs"), readonly).unwrap();

        let before = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
        copy_workspace(&root, &dest, &step("")).unwrap();

        let copied = std::fs::metadata(dest.join("old.rs")).unwrap();
        assert_eq!(std::fs::read(dest.join("old.rs")).unwrap(), b"fn old() {}");
        assert!(copied.permissions().readonly());
        // A build that goes by times must see it as newer than anything it built.
        assert!(
            copied.modified().unwrap() > before,
            "{:?}",
            copied.modified()
        );
    }

    #[test]
    fn the_kept_copy_is_brought_to_the_work_writing_only_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        let (root, dest) = (dir.path().join("work"), dir.path().join("copy"));
        let write = |path: &Path, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(&root.join("src/lib.rs"), "lib\n");
        write(&root.join("src/gone.rs"), "gone\n");
        write(&root.join("tests/t.rs"), "test\n");
        let kept = dir.path().join("copy.json");
        let sync = |stamp: std::time::SystemTime| {
            let copied = Digests::kept(&kept, dir.path().join("fence"));
            let work = Digests::fenced_in(dir.path().join("fence"));
            let written = sync_workspace(&root, &dest, &step(""), &work, &copied, stamp).unwrap();
            copied.keep(&kept).unwrap();
            written
        };
        let modified = |path: &str| {
            std::fs::metadata(dest.join(path))
                .unwrap()
                .modified()
                .unwrap()
        };
        let first = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        assert_eq!(sync(first), 3);
        assert_eq!(modified("src/lib.rs"), first);
        assert_eq!(sync(first + std::time::Duration::from_secs(10)), 0);
        assert_eq!(
            modified("src/lib.rs"),
            first,
            "an unchanged file is left as it is"
        );

        // The work changes; a proof left the copy otherwise.
        write(&root.join("src/lib.rs"), "lib, changed\n");
        std::fs::remove_file(root.join("src/gone.rs")).unwrap();
        write(&dest.join("tests/t.rs"), "TEST\n");
        write(&dest.join("junk/out.txt"), "junk\n");
        write(&dest.join("stray.txt"), "stray\n");
        #[cfg(unix)]
        {
            std::fs::remove_file(dest.join("src/lib.rs")).unwrap();
            std::os::unix::fs::symlink(root.join("src/lib.rs"), dest.join("src/lib.rs")).unwrap();
        }
        let second = first + std::time::Duration::from_secs(20);
        assert_eq!(sync(second), 2);
        for (path, text) in [("src/lib.rs", "lib, changed\n"), ("tests/t.rs", "test\n")] {
            assert_eq!(std::fs::read_to_string(dest.join(path)).unwrap(), text);
            assert_eq!(modified(path), second, "{path}");
            assert!(!std::fs::symlink_metadata(dest.join(path))
                .unwrap()
                .is_symlink());
        }
        for path in ["src/gone.rs", "junk", "stray.txt"] {
            assert!(!dest.join(path).exists(), "{path}");
        }

        // Changed in place, its size and modification time as they were.
        std::fs::write(dest.join("tests/t.rs"), "tset\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(dest.join("tests/t.rs"))
            .unwrap()
            .set_modified(second)
            .unwrap();
        assert_eq!(sync(second), 1);
        assert_eq!(
            std::fs::read_to_string(dest.join("tests/t.rs")).unwrap(),
            "test\n"
        );

        // A file the proof replaced with a directory, and the other way round.
        std::fs::remove_file(dest.join("tests/t.rs")).unwrap();
        std::fs::create_dir_all(dest.join("tests/t.rs/inner")).unwrap();
        std::fs::remove_dir_all(dest.join("src")).unwrap();
        write(&dest.join("src"), "not a directory\n");
        assert_eq!(sync(second), 2);
        assert_eq!(
            std::fs::read_to_string(dest.join("tests/t.rs")).unwrap(),
            "test\n"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("src/lib.rs")).unwrap(),
            "lib, changed\n"
        );
    }
}
