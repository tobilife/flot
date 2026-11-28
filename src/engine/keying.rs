//! Step identity: which facts decide whether a step has to run again.
//!
//! A step key is a digest over the command, the declared environment, the
//! proof command, the content of every declared input file and of every file
//! the proof reads, and the receipt ids of upstream steps. Equal keys mean the same work over the same facts.
//! Any change to what goes into the key must bump `SCHEMA_VERSION`, because it
//! changes every identity flot has ever recorded.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::core::digest;
use crate::core::flow::{Step, SCHEMA_VERSION};
use crate::core::receipt::{DepRef, FileDigest, ToolIdentity};

mod copy;
mod copy_ignore;
mod diff;
mod digests;
mod files;
mod scan;
mod walk;

pub use copy::{copy_workspace, sync_workspace};
pub use diff::{changed_paths, changes, out_of_scope};
pub use digests::Digests;
pub use files::{content_of, is_executable, match_options, within_root};
pub use scan::{scan_globs, scan_outputs, snapshot, Scan};
pub use walk::Reach;

use files::{executable, read_file};
use scan::{scan_in, Keep};

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
    let (mut verify_inputs, proof_limitations) = digest_in(
        root,
        origin,
        &step.verify_inputs,
        "verify input",
        pending,
        digests,
    );
    limitations.extend(proof_limitations);
    // The mutants' allow list is read by the proof as much. None is a list
    // that allows nothing, which is not a limitation; one it cannot read is
    // refused where the report is judged.
    if let Some(allow) = step
        .mutants
        .as_ref()
        .and_then(|mutants| mutants.allow.as_ref())
    {
        let (allow, _) = digest_in(
            root,
            origin,
            std::slice::from_ref(allow),
            "verify input",
            pending,
            digests,
        );
        for file in allow {
            if !verify_inputs.iter().any(|known| known.path == file.path) {
                verify_inputs.push(file);
            }
        }
        verify_inputs.sort_by(|a, b| a.path.cmp(&b.path));
    }
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
    if let Some(mutants) = &step.mutants {
        key["mutants"] = serde_json::json!(mutants);
    }
    if !step.held.is_empty() {
        key["held"] = serde_json::json!(step.held);
    }
    digest::canonical(&key)
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
mod tests;
