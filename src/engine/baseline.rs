//! Proofs checked against a committed revision.
//!
//! An agent that can edit files can edit the check that judges it, and
//! `verify_inputs` only catches a step that does so while it runs. A baseline
//! names a git revision the agent does not control, such as `origin/main`:
//! a step's proof (its claims, its verify command, the patterns of the files
//! that command reads, and those files) must match that revision, or its
//! claims stay unverified and the proof is not run. `flot verify` holds a
//! recorded run to the same revision.

mod changes;
mod drift;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::core::digest;
use crate::core::flow::Flow;
use crate::engine::git::Git;
use crate::engine::keying;

pub use drift::Drift;

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

    /// The files at the revision that `patterns` match.
    fn held_at<'s>(&'s self, patterns: &[String]) -> impl Iterator<Item = (&'s String, &'s Blob)> {
        let patterns: Vec<glob::Pattern> = patterns
            .iter()
            .filter_map(|pattern| glob::Pattern::new(pattern).ok())
            .collect();
        self.tree.iter().filter(move |(path, _)| {
            patterns
                .iter()
                .any(|pattern| pattern.matches_with(path, keying::match_options()))
        })
    }

    /// The digest of the files `patterns` match at the revision: each one's
    /// path, mode and content. A verdict on them holds while it is the same.
    pub fn held_base(&self, patterns: &[String]) -> String {
        let listed: String = self
            .held_at(patterns)
            .map(|(path, blob)| format!("{} {} {path}\n", blob.mode, blob.id))
            .collect();
        digest::bytes(listed.as_bytes())
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
