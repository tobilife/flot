//! Running `git` for the features that build on a repository: sharing runs
//! through a remote, and checking proofs against a committed revision. flot
//! has no network code of its own; a remote is reached by git, with the URL,
//! credentials and transport git already uses for it.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::core::fault;

pub struct Git<'a> {
    root: &'a Path,
}

impl<'a> Git<'a> {
    pub fn open(root: &'a Path) -> Result<Self> {
        let git = Git { root };
        git.run(&["rev-parse", "--git-dir"], None)
            .map_err(|error| {
                fault::environment(format!("the flow is not in a git repository: {error:#}"))
            })?;
        Ok(git)
    }

    /// Run git here and return its trimmed stdout, or its stderr as the error.
    pub fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<String> {
        let bytes = self.output(args, input, false)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_string())
    }

    /// A git command that may reach a remote. It must never wait for a
    /// password nobody can type: ask git to fail instead.
    pub fn network(&self, args: &[&str]) -> Result<String> {
        let bytes = self.output(args, None, true)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_string())
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command.arg("-C").arg(self.root).args(args);
        command
    }

    pub fn output(&self, args: &[&str], input: Option<&[u8]>, network: bool) -> Result<Vec<u8>> {
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
    pub fn commit_tree(&self, args: &[&str]) -> Result<String> {
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

    /// Every file under a commit: path to blob id.
    pub fn tree(&self, commit: &str) -> Result<BTreeMap<String, String>> {
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
    pub fn make_tree(&self, files: &BTreeMap<String, String>) -> Result<String> {
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

    /// One blob's bytes, refused when larger than `limit`.
    pub fn blob(&self, id: &str, limit: u64) -> Result<Vec<u8>> {
        let size: u64 = self
            .run(&["cat-file", "-s", id], None)?
            .parse()
            .context("git reported a size that is not a number")?;
        if size > limit {
            bail!("the object is {size} bytes; flot reads up to {limit} bytes");
        }
        self.output(&["cat-file", "blob", id], None, false)
    }
}
