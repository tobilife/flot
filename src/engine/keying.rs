//! Step identity: which facts decide whether a step has to run again.
//!
//! A step key is a digest over the command, the declared environment, the
//! proof command, the content of every declared input file and of every file
//! the proof reads, and the receipt ids of upstream steps. Equal keys mean the same work over the same facts.
//! Any change to what goes into the key must bump `SCHEMA_VERSION`, because it
//! changes every identity flot has ever recorded.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result};

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

/// Like `facts_of`, as the files will be once `pending` are in place: files
/// that are missing now but that reusing an upstream receipt will restore.
/// `plan` needs this to predict what `run` will see.
pub fn facts_with(root: &Path, step: &Step, pending: &[FileDigest]) -> Facts {
    let (inputs, mut limitations) = digest_globs(root, &step.inputs, "input", pending);
    let (verify_inputs, proof_limitations) =
        digest_globs(root, &step.verify_inputs, "verify input", pending);
    limitations.extend(proof_limitations);
    if step.inputs.is_empty() {
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

/// Resolve globs, relative to `root`, handing each matched file's bytes to
/// `keep`, which returns the digest to record for it. Files are read one at a
/// time so a large tree is never held in memory at once.
pub fn scan_globs<E>(
    root: &Path,
    patterns: &[String],
    kind: &str,
    mut keep: impl FnMut(&[u8]) -> Result<String, E>,
) -> Result<Scan, E> {
    let mut files: BTreeMap<String, (String, bool)> = BTreeMap::new();
    let mut scan = Scan::default();
    let Some(root_text) = root.to_str() else {
        scan.problems.push(format!(
            "the flow root {} is not valid UTF-8",
            root.display()
        ));
        scan.unmatched = patterns.to_vec();
        return Ok(scan);
    };
    // The root is a literal path; only the pattern is a pattern.
    let escaped_root = glob::Pattern::escape(root_text);
    for pattern in patterns {
        let absolute = format!("{}/{}", escaped_root.trim_end_matches('/'), pattern);
        let entries = match glob::glob(&absolute) {
            Ok(entries) => entries,
            Err(error) => {
                scan.problems
                    .push(format!("{kind} pattern `{pattern}` is invalid: {error}"));
                continue;
            }
        };
        let mut matched = 0usize;
        for entry in entries.flatten() {
            if !entry.is_file() {
                continue;
            }
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
                    files.insert(relative, (keep(&bytes)?, is_executable(&entry)));
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
    let mut scan = scan_globs(root, patterns, "output", keep)?;
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
    let mut scan = match scan_globs(root, patterns, kind, |bytes| {
        Ok::<_, std::convert::Infallible>(digest::bytes(bytes))
    }) {
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
    digest::canonical(&serde_json::json!({
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
    }))
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
}
