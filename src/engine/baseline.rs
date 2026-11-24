//! Proofs checked against a committed revision.
//!
//! An agent that can edit files can edit the check that judges it, and
//! `verify_inputs` only catches a step that does so while it runs. A baseline
//! names a git revision the agent does not control, such as `origin/main`:
//! a step's proof (its claims, its verify command, the patterns of the files
//! that command reads, and those files) must match that revision, or its
//! claims stay unverified and the proof is not run. `flot verify` holds a
//! recorded run to the same revision.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::core::digest;
use crate::core::flow::{Flow, Step};
use crate::core::receipt::{ClaimStatus, FileDigest, ReceiptIdentity};
use crate::engine::git::Git;
use crate::engine::keying;

/// Largest proof input read from the baseline revision.
const MAX_FILE_BYTES: u64 = 64 << 20;

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

#[derive(Debug)]
pub struct Baseline {
    /// The revision as named, such as `origin/main`.
    pub rev: String,
    pub commit: String,
    root: PathBuf,
    /// The flow file, relative to `root`.
    flow_file: String,
    /// The flow at the revision; `None` when the flow file is not there or
    /// does not parse.
    flow: Option<Flow>,
    /// Every file at the revision under the flow root, relative to it.
    tree: BTreeMap<String, Blob>,
    /// Digests of the files read so far.
    digests: Mutex<BTreeMap<String, String>>,
}

/// A file at the revision.
#[derive(Debug)]
struct Blob {
    /// Its git mode, such as `100644`.
    mode: String,
    /// Its git object id.
    id: String,
}

/// Why a step's proof does not match the baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    pub reasons: Vec<String>,
    /// Every claim the step makes here or made at the revision, so a claim
    /// dropped from the flow still shows as unproven.
    pub claims: Vec<String>,
}

/// What a proof is, wherever it is read from: a step of the flow being run,
/// or a receipt a run recorded.
struct Proof<'a> {
    step_id: &'a str,
    claims: Vec<&'a str>,
    verify: Option<&'a str>,
    /// `None` when the source does not record them, as a receipt does not.
    patterns: Option<&'a [String]>,
    files: &'a [FileDigest],
    /// For a step, whether it declares `fails_before`.
    fails_before: Option<bool>,
    /// For a receipt, the commit its proof was run before the change at, if any.
    checked_at: Option<&'a str>,
    /// Whether a claim was verified: only then does how it was matter.
    verified: bool,
}

impl Baseline {
    /// Read the flow file `flow_file` (a path relative to `root`) and the
    /// file list as they are at `rev`.
    pub fn load(root: &Path, flow_file: &str, rev: &str) -> Result<Baseline> {
        let git = Git::open(root).context("checking proofs against a revision needs git")?;
        let commit = git
            .run(&["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")], None)
            .map_err(|_| {
                crate::core::fault::environment(format!(
                    "proofs are checked against `{rev}`, which is not a commit in this repository; fetch it or name another revision"
                ))
            })?;
        let prefix = git.run(&["rev-parse", "--show-prefix"], None)?;
        let listed = git.output(
            &["ls-tree", "-r", "-z", "--full-tree", &commit],
            None,
            false,
        )?;
        let mut tree: BTreeMap<String, Blob> = BTreeMap::new();
        for entry in listed
            .split(|&byte| byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let entry = String::from_utf8_lossy(entry);
            let Some((meta, path)) = entry.split_once('\t') else {
                continue;
            };
            let mut meta = meta.split(' ');
            if let (Some(mode), Some("blob"), Some(id)) = (meta.next(), meta.next(), meta.next()) {
                if let Some(relative) = path.strip_prefix(prefix.as_str()) {
                    tree.insert(
                        relative.to_string(),
                        Blob {
                            mode: mode.to_string(),
                            id: id.to_string(),
                        },
                    );
                }
            }
        }
        let flow = match tree.get(flow_file) {
            Some(blob) => String::from_utf8(git.blob(&blob.id, MAX_FILE_BYTES)?)
                .ok()
                .and_then(|text| Flow::parse(&text).ok()),
            None => None,
        };
        Ok(Baseline {
            rev: rev.to_string(),
            commit,
            root: root.to_path_buf(),
            flow_file: flow_file.to_string(),
            flow,
            tree,
            digests: Mutex::default(),
        })
    }

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
            let target = copy.join(path);
            let present = std::fs::symlink_metadata(&target).ok();
            let Some(blob) = self.tree.get(path) else {
                // A directory here is a nested repository, not a file.
                if present.is_some_and(|meta| !meta.is_dir()) {
                    std::fs::remove_file(&target)
                        .with_context(|| format!("cannot remove {path}"))?;
                }
                continue;
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

    /// How the run names the baseline in its limitations.
    pub fn label(&self) -> String {
        format!(
            "{} ({})",
            self.rev,
            &self.commit[..self.commit.len().min(12)]
        )
    }

    /// Steps with claims at the revision that are not among `present`.
    pub fn missing_steps(&self, present: impl Fn(&str) -> bool) -> Vec<String> {
        let Some(old) = &self.flow else {
            return Vec::new();
        };
        old.steps
            .iter()
            .filter(|step| !step.claims.is_empty() && !present(&step.id))
            .map(|step| step.id.clone())
            .collect()
    }

    /// Why `step`'s proof differs from the revision, given the proof inputs
    /// it reads now; `None` when it matches, or when there is nothing to
    /// prove here or there.
    pub fn drift(&self, step: &Step, verify_inputs: &[FileDigest]) -> Result<Option<Drift>> {
        self.compare(&Proof {
            step_id: &step.id,
            claims: step.claims.iter().map(String::as_str).collect(),
            verify: step.verify.as_deref(),
            patterns: Some(&step.verify_inputs),
            files: verify_inputs,
            fails_before: Some(step.fails_before),
            checked_at: None,
            verified: false,
        })
    }

    /// Why the proof a receipt recorded differs from the revision. A receipt
    /// does not record its `verify_inputs` patterns, so only the files they
    /// matched are compared.
    pub fn drift_of(&self, identity: &ReceiptIdentity) -> Result<Option<Drift>> {
        self.compare(&Proof {
            step_id: &identity.step_id,
            claims: identity
                .claims
                .iter()
                .map(|claim| claim.text.as_str())
                .collect(),
            verify: identity
                .claims
                .iter()
                .find_map(|claim| claim.verify_command.as_deref()),
            patterns: None,
            files: &identity.verify_inputs,
            fails_before: None,
            checked_at: identity
                .claims
                .iter()
                .find_map(|claim| claim.before.as_ref())
                .map(|before| before.commit.as_str()),
            verified: identity
                .claims
                .iter()
                .any(|claim| claim.status == ClaimStatus::Verified),
        })
    }

    fn compare(&self, proof: &Proof) -> Result<Option<Drift>> {
        let old = self.flow.as_ref().and_then(|old| old.step(proof.step_id));
        let old_claims: Vec<&str> = old
            .map(|old| old.claims.iter().map(String::as_str).collect())
            .unwrap_or_default();
        if proof.claims.is_empty() && old_claims.is_empty() {
            return Ok(None);
        }
        let mut reasons = Vec::new();
        match old {
            None if self.flow.is_none() => {
                reasons.push("the flow file is not there".to_string());
            }
            None => reasons.push(format!("step `{}` is not there", proof.step_id)),
            Some(old) => {
                if old_claims != proof.claims {
                    reasons.push("the step's claims differ".into());
                }
                if old.verify.as_deref() != proof.verify {
                    reasons.push("its verify command differs".into());
                }
                if proof
                    .patterns
                    .is_some_and(|patterns| patterns != old.verify_inputs.as_slice())
                {
                    reasons.push("its verify_inputs differ".into());
                }
                match proof.fails_before {
                    Some(declared) if declared != old.fails_before => {
                        reasons.push("its fails_before differs".into());
                    }
                    Some(_) => {}
                    // A receipt held to the revision's fails_before: what
                    // it verified, it must have seen fail at the revision.
                    None if old.fails_before
                        && proof.verified
                        && proof.checked_at != Some(self.commit.as_str()) =>
                    {
                        reasons.push(match proof.checked_at {
                            None => "its proof was not run before the change".to_string(),
                            Some(commit) => format!(
                                "its proof was run before the change at {}, not at this revision",
                                &commit[..commit.len().min(12)]
                            ),
                        });
                    }
                    None => {}
                }
            }
        }
        for file in proof.files {
            match self.digest(&file.path)? {
                None => reasons.push(format!("{} is not there", file.path)),
                Some(digest) if digest != file.digest => {
                    reasons.push(format!("{} differs", file.path))
                }
                Some(_) => {}
            }
        }
        // What the revision says the proof reads, and is not read here.
        let read: BTreeSet<&str> = proof.files.iter().map(|file| file.path.as_str()).collect();
        let patterns: Vec<glob::Pattern> = old
            .iter()
            .flat_map(|old| &old.verify_inputs)
            .filter_map(|pattern| glob::Pattern::new(pattern).ok())
            .collect();
        for path in self.tree.keys() {
            if !read.contains(path.as_str())
                && patterns
                    .iter()
                    .any(|pattern| pattern.matches_with(path, keying::match_options()))
            {
                reasons.push(format!("{path} is there but missing here"));
            }
        }
        if reasons.is_empty() {
            return Ok(None);
        }
        let mut claims: Vec<String> = proof.claims.iter().map(|claim| claim.to_string()).collect();
        for claim in old_claims {
            if !proof.claims.contains(&claim) {
                claims.push(claim.to_string());
            }
        }
        Ok(Some(Drift { reasons, claims }))
    }

    /// The digest of `path` at the revision; `None` when it is not there.
    fn digest(&self, path: &str) -> Result<Option<String>> {
        let Some(Blob { id, .. }) = self.tree.get(path) else {
            return Ok(None);
        };
        if let Some(known) = self.digests.lock().expect("digest cache").get(path) {
            return Ok(Some(known.clone()));
        }
        let bytes = Git::open(&self.root)?
            .blob(id, MAX_FILE_BYTES)
            .with_context(|| format!("cannot read {path} at {}", self.label()))?;
        let found = digest::bytes(&bytes);
        self.digests
            .lock()
            .expect("digest cache")
            .insert(path.to_string(), found.clone());
        Ok(Some(found))
    }
}
