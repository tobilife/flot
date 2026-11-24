//! Share runs through a git remote.
//!
//! flot has no network code of its own: `push` and `pull` run `git`, so a
//! remote is reached with whatever URL, credentials and transport git already
//! uses for it. Runs travel as bundles in a ref of their own,
//! `refs/flot/runs`, one file per run under the flow's name
//! (`<flow>/<run-id>.flotbundle.json`). The ref is never checked out and never
//! touches a branch, the index or the working tree.
//!
//! A pulled bundle is imported like `flot import`: authenticated in full, and
//! reusable only when a signer this host trusts signed it.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::core::fault;
use crate::core::receipt::RunManifest;
use crate::service::bundle::{self, Bundle, ImportPolicy, ImportReport, MAX_BUNDLE_BYTES};
use crate::service::FaultKind;
use crate::store::Store;

/// The ref runs are shared under, on every remote.
pub const RUNS_REF: &str = "refs/flot/runs";

/// How often a push starts over after another push won the race.
const PUSH_TRIES: usize = 3;

#[derive(Debug, Clone, Serialize)]
pub struct PushReport {
    pub remote: String,
    pub run_id: String,
    /// The file the bundle is kept in under the ref.
    pub path: String,
    /// The commit the remote's ref now points to.
    pub commit: String,
    /// The remote already held this exact bundle; nothing was pushed.
    pub unchanged: bool,
    pub signed_by: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PullReport {
    pub remote: String,
    pub flow: String,
    /// The remote has no shared runs at all.
    pub empty: bool,
    pub imported: Vec<ImportReport>,
    /// Runs this store already holds; they were not imported again.
    pub present: Vec<String>,
    /// Bundles that could not be imported, with why.
    pub refused: Vec<Refusal>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Refusal {
    pub path: String,
    pub reason: String,
    pub kind: FaultKind,
}

/// Put one run's bundle on `remote`, keeping every run already there.
pub fn push(
    root: &Path,
    remote: &str,
    manifest: &RunManifest,
    bundle: &Bundle,
) -> Result<PushReport> {
    let git = Git::open(root)?;
    let path = format!("{}/{}.flotbundle.json", manifest.flow, manifest.run_id);
    check_path(&path)?;
    let text = serde_json::to_string_pretty(bundle)?;
    let blob = git.run(&["hash-object", "-w", "--stdin"], Some(text.as_bytes()))?;
    let mut tries = 0;
    loop {
        tries += 1;
        let tip = git.fetch(remote)?;
        let mut tree = match &tip {
            Some(tip) => git.tree(tip)?,
            None => BTreeMap::new(),
        };
        if let Some(tip) = &tip {
            if tree.get(&path) == Some(&blob) {
                return Ok(PushReport {
                    remote: remote.to_string(),
                    run_id: manifest.run_id.clone(),
                    path,
                    commit: tip.clone(),
                    unchanged: true,
                    signed_by: signers(bundle),
                });
            }
        }
        tree.insert(path.clone(), blob.clone());
        let tree = git.make_tree(&tree)?;
        let message = format!("flot: {} {}", manifest.flow, manifest.run_id);
        let mut args = vec!["commit-tree", tree.as_str(), "-m", message.as_str()];
        if let Some(tip) = &tip {
            args.extend(["-p", tip.as_str()]);
        }
        let commit = git.commit_tree(&args)?;
        let refspec = format!("{commit}:{RUNS_REF}");
        match git.network(&["push", "--quiet", remote, &refspec]) {
            Ok(_) => {
                return Ok(PushReport {
                    remote: remote.to_string(),
                    run_id: manifest.run_id.clone(),
                    path,
                    commit,
                    unchanged: false,
                    signed_by: signers(bundle),
                })
            }
            // Someone else pushed first: build on theirs and try again.
            Err(_) if tries < PUSH_TRIES && git.fetch(remote)? != tip => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Import every run of `flow` on `remote` that this store does not hold.
pub fn pull(
    root: &Path,
    store: &Store,
    remote: &str,
    flow: &str,
    policy: ImportPolicy,
) -> Result<PullReport> {
    let git = Git::open(root)?;
    let mut report = PullReport {
        remote: remote.to_string(),
        flow: flow.to_string(),
        ..PullReport::default()
    };
    let Some(tip) = git.fetch(remote)? else {
        report.empty = true;
        return Ok(report);
    };
    let prefix = format!("{flow}/");
    for (path, blob) in git.tree(&tip)? {
        let Some(name) = path.strip_prefix(&prefix) else {
            continue;
        };
        let Some(run_id) = name.strip_suffix(".flotbundle.json") else {
            continue;
        };
        if store
            .resolve_run(run_id)
            .is_ok_and(|run| run.run_id == run_id)
        {
            report.present.push(run_id.to_string());
            continue;
        }
        let imported = git.blob(&blob).and_then(|text| {
            let bundle = bundle::parse(&text).context("not a flot bundle")?;
            if bundle.run.run_id != run_id || bundle.run.flow != flow {
                return Err(fault::integrity(format!(
                    "the bundle carries run {} of flow `{}`, not the run its name says",
                    bundle.run.run_id, bundle.run.flow
                )));
            }
            bundle::import(store, &bundle, policy)
        });
        match imported {
            Ok(imported) => report.imported.push(imported),
            Err(error) => report.refused.push(Refusal {
                path,
                kind: super::fault_kind(&error),
                reason: format!("{error:#}"),
            }),
        }
    }
    Ok(report)
}

fn signers(bundle: &Bundle) -> Vec<String> {
    bundle
        .attestations
        .iter()
        .map(|attestation| attestation.key.clone())
        .collect()
}

/// A path flot itself would write under the ref: no empty, `.` or `..`
/// components, which a flow name could otherwise smuggle in.
fn check_path(path: &str) -> Result<()> {
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        bail!("cannot share a run of a flow named so that its path would be `{path}`");
    }
    Ok(())
}

struct Git<'a> {
    root: &'a Path,
}

impl<'a> Git<'a> {
    fn open(root: &'a Path) -> Result<Self> {
        let git = Git { root };
        git.run(&["rev-parse", "--git-dir"], None)
            .map_err(|error| {
                fault::environment(format!(
                    "sharing runs needs the flow to be in a git repository: {error:#}"
                ))
            })?;
        Ok(git)
    }

    /// Run git here and return its trimmed stdout, or its stderr as the error.
    fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<String> {
        let bytes = self.output(args, input, false)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_string())
    }

    /// A git command that may reach a remote. It must never wait for a
    /// password nobody can type: ask git to fail instead.
    fn network(&self, args: &[&str]) -> Result<String> {
        let bytes = self.output(args, None, true)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_string())
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command.arg("-C").arg(self.root).args(args);
        command
    }

    fn output(&self, args: &[&str], input: Option<&[u8]>, network: bool) -> Result<Vec<u8>> {
        self.finish(self.command(args), args, input, network)
    }

    fn finish(
        &self,
        mut command: Command,
        args: &[&str],
        input: Option<&[u8]>,
        network: bool,
    ) -> Result<Vec<u8>> {
        command
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if network {
            command.env("GIT_TERMINAL_PROMPT", "0");
        }
        let mut child = command.spawn().map_err(|error| {
            fault::environment(format!(
                "cannot run git: {error}; install git to share runs"
            ))
        })?;
        if let Some(input) = input {
            child
                .stdin
                .take()
                .expect("stdin is piped")
                .write_all(input)
                .context("cannot write to git")?;
        }
        let output = child.wait_with_output().context("cannot run git")?;
        if !output.status.success() {
            let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let error = format!("`git {}` failed: {said}", args.join(" "));
            return Err(if network {
                fault::environment(error)
            } else {
                anyhow::anyhow!(error)
            });
        }
        Ok(output.stdout)
    }

    /// `git commit-tree` as whoever git says commits here; a host with no
    /// git identity, such as a CI runner, commits as `flot`.
    fn commit_tree(&self, args: &[&str]) -> Result<String> {
        let mut command = self.command(args);
        if self.run(&["var", "GIT_COMMITTER_IDENT"], None).is_err() {
            for (key, value) in [
                ("GIT_AUTHOR_NAME", "flot"),
                ("GIT_AUTHOR_EMAIL", "flot@localhost"),
                ("GIT_COMMITTER_NAME", "flot"),
                ("GIT_COMMITTER_EMAIL", "flot@localhost"),
            ] {
                command.env(key, value);
            }
        }
        let bytes = self.finish(command, args, None, false)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_string())
    }

    /// The commit `remote`'s runs ref points to, fetched here, or `None` when
    /// the remote has none.
    fn fetch(&self, remote: &str) -> Result<Option<String>> {
        let listed = self.network(&["ls-remote", remote, RUNS_REF])?;
        if listed.is_empty() {
            return Ok(None);
        }
        // A remote given as a URL is not a valid ref name; name it by digest.
        let name = if remote
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !remote.starts_with('.')
        {
            remote.to_string()
        } else {
            crate::core::digest::short(&crate::core::digest::bytes(remote.as_bytes())).to_string()
        };
        let local = format!("refs/flot/remotes/{name}/runs");
        self.network(&["fetch", "--quiet", remote, &format!("+{RUNS_REF}:{local}")])?;
        Ok(Some(self.run(
            &["rev-parse", &format!("{local}^{{commit}}")],
            None,
        )?))
    }

    /// Every file under a commit: path to blob id.
    fn tree(&self, commit: &str) -> Result<BTreeMap<String, String>> {
        let listed = self.output(&["ls-tree", "-r", "-z", commit], None, false)?;
        let mut files = BTreeMap::new();
        for entry in listed
            .split(|&byte| byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let entry = String::from_utf8_lossy(entry);
            let Some((meta, path)) = entry.split_once('\t') else {
                bail!("unexpected `git ls-tree` line: {entry}");
            };
            let mut meta = meta.split(' ');
            if let (Some(_mode), Some("blob"), Some(id)) = (meta.next(), meta.next(), meta.next()) {
                files.insert(path.to_string(), id.to_string());
            }
        }
        Ok(files)
    }

    /// A tree of nested directories holding `files`.
    fn make_tree(&self, files: &BTreeMap<String, String>) -> Result<String> {
        let mut here: BTreeMap<String, String> = BTreeMap::new();
        let mut below: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for (path, blob) in files {
            match path.split_once('/') {
                Some((dir, rest)) => {
                    below
                        .entry(dir.to_string())
                        .or_default()
                        .insert(rest.to_string(), blob.clone());
                }
                None => {
                    here.insert(path.clone(), format!("100644 blob {blob}"));
                }
            }
        }
        for (dir, files) in below {
            let tree = self.make_tree(&files)?;
            here.insert(dir, format!("040000 tree {tree}"));
        }
        let mut listing = Vec::new();
        for (name, entry) in here {
            listing.extend_from_slice(format!("{entry}\t{name}").as_bytes());
            listing.push(0);
        }
        self.run(&["mktree", "-z"], Some(&listing))
    }

    /// One blob's content, refused when larger than a bundle may be.
    fn blob(&self, id: &str) -> Result<String> {
        let size: u64 = self
            .run(&["cat-file", "-s", id], None)?
            .parse()
            .context("git reported a size that is not a number")?;
        if size > MAX_BUNDLE_BYTES {
            bail!("the bundle is {size} bytes; flot reads bundles up to {MAX_BUNDLE_BYTES} bytes");
        }
        let bytes = self.output(&["cat-file", "blob", id], None, false)?;
        String::from_utf8(bytes).context("the bundle is not UTF-8")
    }
}
