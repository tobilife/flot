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
use crate::core::receipt::{FileDigest, ReceiptIdentity};
use crate::engine::git::Git;
use crate::engine::keying;

/// Largest proof input read from the baseline revision.
const MAX_FILE_BYTES: u64 = 64 << 20;

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
    /// Every file at the revision under the flow root, relative to it, with
    /// its git object id.
    tree: BTreeMap<String, String>,
    /// Digests of the files read so far.
    digests: Mutex<BTreeMap<String, String>>,
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
        let mut tree: BTreeMap<String, String> = BTreeMap::new();
        for entry in listed
            .split(|&byte| byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let entry = String::from_utf8_lossy(entry);
            let Some((meta, path)) = entry.split_once('\t') else {
                continue;
            };
            let mut meta = meta.split(' ');
            if let (Some(_), Some("blob"), Some(id)) = (meta.next(), meta.next(), meta.next()) {
                if let Some(relative) = path.strip_prefix(prefix.as_str()) {
                    tree.insert(relative.to_string(), id.to_string());
                }
            }
        }
        let flow = match tree.get(flow_file) {
            Some(id) => String::from_utf8(git.blob(id, MAX_FILE_BYTES)?)
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
        let listed = |args: &[&str]| -> Result<Vec<String>> {
            let bytes = git.output(args, None, false)?;
            Ok(bytes
                .split(|&byte| byte == 0)
                .filter(|path| !path.is_empty())
                .map(|path| String::from_utf8_lossy(path).into_owned())
                .collect())
        };
        let mut changed: BTreeSet<String> = listed(&[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--relative",
            &self.commit,
        ])?
        .into_iter()
        .collect();
        let untracked = ["ls-files", "-z", "--others", "--exclude-standard"];
        for path in listed(&[&untracked[..], &["--directory"]].concat())? {
            if path.ends_with('/') && read.iter().any(|file| file.starts_with(&path)) {
                changed.extend(listed(&[&untracked[..], &["--", &path]].concat())?);
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
        let Some(id) = self.tree.get(path) else {
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
