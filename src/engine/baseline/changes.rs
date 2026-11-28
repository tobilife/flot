//! What differs here from the revision, and putting files in a copy back as
//! the revision has them.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};

use crate::core::receipt::FileDigest;
use crate::engine::git::Git;

use super::{Baseline, Blob, MAX_FILE_BYTES};

/// Lists the files under the working directory git sees and does not track.
const UNTRACKED: [&str; 4] = ["ls-files", "-z", "--others", "--exclude-standard"];

/// The paths a git command lists, NUL-separated.
fn listed(git: &Git, args: &[&str]) -> Result<Vec<String>> {
    let bytes = git.output(args, None, false)?;
    Ok(bytes
        .split(|&byte| byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

impl Baseline {
    /// Files under the flow root that differ here from the revision (edited,
    /// added or deleted, tracked or not ignored) and that `read` does not
    /// hold, relative to the root. The flow file is left out; its proofs are
    /// compared step by step. An untracked directory none of whose files was
    /// read is named once, as `dir/`.
    pub fn unread(&self, read: &BTreeSet<String>) -> Result<Vec<String>> {
        let git = Git::open(&self.root)?;
        let mut changed = self.edited(&git)?;
        for path in listed(&git, &[&UNTRACKED[..], &["--directory"]].concat())? {
            if path.ends_with('/') && read.iter().any(|file| file.starts_with(&path)) {
                changed.extend(listed(&git, &[&UNTRACKED[..], &["--", &path]].concat())?);
            } else {
                changed.insert(path);
            }
        }
        changed.remove(&self.flow_file);
        Ok(changed
            .into_iter()
            .filter(|path| !read.contains(path))
            .collect())
    }

    /// Undo, in `copy`, a copy of the flow root, every change under the flow
    /// root here since the revision: an edited or deleted file gets its
    /// content and mode at the revision back, a file the revision does not
    /// have is removed. What git ignores is left as it is.
    pub fn undo(&self, copy: &Path) -> Result<()> {
        let git = Git::open(&self.root)?;
        let mut changed = self.edited(&git)?;
        changed.extend(listed(&git, &UNTRACKED)?);
        for path in &changed {
            self.put_back(&git, copy, path)?;
        }
        Ok(())
    }

    /// Put `path` in `copy` as the revision has it: its content and mode, or
    /// removed when the revision has no such file.
    fn put_back(&self, git: &Git, copy: &Path, path: &str) -> Result<()> {
        let target = copy.join(path);
        let present = std::fs::symlink_metadata(&target).ok();
        let Some(blob) = self.tree.get(path) else {
            // A directory here is a nested repository, not a file.
            if present.is_some_and(|meta| !meta.is_dir()) {
                std::fs::remove_file(&target).with_context(|| format!("cannot remove {path}"))?;
            }
            return Ok(());
        };
        // A link would let a later write land outside the copy.
        if blob.mode == "120000" {
            anyhow::bail!("{path} is a symbolic link at {}", self.label());
        }
        let bytes = git
            .blob(&blob.id, MAX_FILE_BYTES)
            .with_context(|| format!("cannot read {path} at {}", self.label()))?;
        match present {
            Some(meta) if meta.is_dir() => std::fs::remove_dir_all(&target),
            _ => Ok(()),
        }
        .with_context(|| format!("cannot replace {path}"))?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        std::fs::write(&target, bytes).with_context(|| format!("cannot write {path}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if blob.mode == "100755" { 0o755 } else { 0o644 };
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                .with_context(|| format!("cannot set the mode of {path}"))?;
        }
        Ok(())
    }

    /// The files `patterns` match that differ here from the revision, given
    /// what they hold here: edited, gone, or not at the revision.
    pub fn held_changes(&self, patterns: &[String], here: &[FileDigest]) -> Result<Vec<String>> {
        let mut changed = BTreeSet::new();
        let at: Vec<(&String, &Blob)> = self.held_at(patterns).collect();
        for (path, blob) in &at {
            let same = match here.iter().find(|file| &file.path == *path) {
                None => false,
                Some(file) => {
                    self.digest(path)?.as_deref() == Some(file.digest.as_str())
                        && (blob.mode == "100755") == file.executable
                }
            };
            if !same {
                changed.insert(path.to_string());
            }
        }
        for file in here {
            if !at.iter().any(|(path, _)| **path == file.path) {
                changed.insert(file.path.clone());
            }
        }
        Ok(changed.into_iter().collect())
    }

    /// Put each of `paths` in `copy` as the revision has it.
    pub fn hold(&self, copy: &Path, paths: &[String]) -> Result<()> {
        let git = Git::open(&self.root)?;
        for path in paths {
            self.put_back(&git, copy, path)?;
        }
        Ok(())
    }

    /// Tracked files under the flow root that differ here from the revision,
    /// relative to it, edited, added or deleted.
    fn edited(&self, git: &Git) -> Result<BTreeSet<String>> {
        Ok(listed(
            git,
            &[
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                "--relative",
                &self.commit,
            ],
        )?
        .into_iter()
        .collect())
    }
}
