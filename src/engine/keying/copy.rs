//! Copies of the project a step runs in: made for a candidate, or kept and
//! brought to the work again for each proof.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::core::flow::Step;

use super::scan::digest_all;
use super::walk::{candidates, Reach};
use super::Digests;

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
        (&step.proof_inputs(), Reach::Tracked),
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

#[cfg(test)]
mod tests;
