//! The project's ignore rules, applied to a copy of it in the store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// The ignore rules of a project, applied to a copy of it.
///
/// A copy lives in the store, whose `.gitignore` ignores everything, and
/// what lies above the store is the project and its parents, not the copy's:
/// read from there, the rules would hide every file of the copy. Here they
/// are what a scan of the project itself reads, in the same order: the
/// copy's own `.gitignore` files, those of the directories above the project,
/// `info/exclude` of every repository at or above each of them, and the
/// user's global excludes. The first one that matches decides.
pub(super) struct CopyIgnore {
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
    pub(super) fn new(copy: &Path, origin: &Path) -> Self {
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
    pub(super) fn ignores(&self, path: &Path, is_dir: bool) -> bool {
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

#[cfg(test)]
mod tests;
