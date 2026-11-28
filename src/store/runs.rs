//! Run manifests: recording a run, here or from a bundle, listing them, and
//! resolving a run selector to one.

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use super::{write_atomic, Store};
use crate::core::fault;
use crate::core::receipt::RunManifest;

impl Store {
    /// Run ids name files, so anything beyond the alphabet flot itself
    /// produces is refused before it reaches a path.
    pub(super) fn run_file(&self, dir: &str, run_id: &str) -> Result<PathBuf> {
        if !is_run_id(run_id) {
            bail!("`{run_id}` is not a run id");
        }
        Ok(self.root.join(dir).join(format!("{run_id}.json")))
    }

    fn run_path(&self, run_id: &str) -> Result<PathBuf> {
        self.run_file("runs", run_id)
    }

    /// Record a run this host executed. It becomes `latest`.
    pub fn save_run(&self, manifest: &RunManifest) -> Result<()> {
        self.ensure()?;
        write_atomic(
            &self.run_path(&manifest.run_id)?,
            serde_json::to_string_pretty(manifest)?.as_bytes(),
        )?;
        write_atomic(&self.root.join("latest"), manifest.run_id.as_bytes())
    }

    /// Record a run that arrived in a bundle. It is marked as imported and
    /// never becomes `latest`: commands that default to the latest run mean
    /// this host's work.
    pub fn save_imported_run(&self, manifest: &RunManifest) -> Result<()> {
        self.ensure()?;
        write_atomic(&self.run_file("imported", &manifest.run_id)?, b"")?;
        write_atomic(
            &self.run_path(&manifest.run_id)?,
            serde_json::to_string_pretty(manifest)?.as_bytes(),
        )
    }

    /// Whether a run arrived in a bundle rather than ran here.
    pub fn is_imported(&self, run_id: &str) -> bool {
        self.run_file("imported", run_id)
            .is_ok_and(|path| path.exists())
    }

    /// When the newest run this host executed started, read off its id: the
    /// run that `latest~N` and the run list count back from.
    pub fn newest_local_start_ms(&self) -> Result<Option<u64>> {
        Ok(self
            .list_runs()?
            .iter()
            .filter(|id| !self.is_imported(id))
            .find_map(|id| id.get(..13)?.parse().ok()))
    }

    pub fn list_runs(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let dir = self.root.join("runs");
        if !dir.exists() {
            return Ok(ids);
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_suffix(".json") {
                if is_run_id(id) {
                    ids.push(id.to_string());
                }
            }
        }
        // Run ids start with a zero-padded millisecond stamp, so lexical order
        // is chronological order.
        ids.sort();
        ids.reverse();
        Ok(ids)
    }

    /// Resolve `latest` (the newest run this host executed), `latest~N` (the
    /// run executed here N runs before it), an exact run id, or a prefix of at
    /// least `MIN_RUN_PREFIX` characters of a run id or of the root digest it
    /// ends with.
    pub fn resolve_run(&self, spec: &str) -> Result<RunManifest> {
        let ids = self.list_runs()?;
        if ids.is_empty() {
            bail!("no runs recorded yet — run `flot run` first");
        }
        let back = match spec.strip_prefix("latest") {
            Some("") => Some(0),
            Some(rest) => match rest.strip_prefix('~').map(str::parse::<usize>) {
                Some(Ok(back)) => Some(back),
                _ => bail!("`{spec}` is not a run selector; use `latest` or `latest~N`"),
            },
            None => None,
        };
        let run_id = if let Some(back) = back {
            let pointed = fs::read_to_string(self.root.join("latest"))
                .map(|text| text.trim().to_string())
                .ok()
                .filter(|id| ids.contains(id));
            let local: Vec<&String> = ids.iter().filter(|id| !self.is_imported(id)).collect();
            let newest = match pointed {
                Some(id) => local.iter().position(|local| **local == id).unwrap_or(0),
                None => 0,
            };
            if local.is_empty() {
                bail!("no run was executed here yet; name an imported run by its id");
            }
            match local.get(newest + back) {
                Some(id) => (*id).clone(),
                None => bail!(
                    "`{spec}` goes back further than the {} run(s) executed here",
                    local.len() - newest
                ),
            }
        } else if ids.iter().any(|id| id == spec) {
            spec.to_string()
        } else {
            if spec.chars().count() < MIN_RUN_PREFIX {
                bail!("`{spec}` is too short to name a run; give at least {MIN_RUN_PREFIX} characters of its id or root");
            }
            let matches: Vec<&String> = ids
                .iter()
                .filter(|id| {
                    id.starts_with(spec)
                        || id
                            .split_once('-')
                            .is_some_and(|(_, root)| root.starts_with(spec))
                })
                .collect();
            match matches.len() {
                0 => bail!("no run id or root starts with `{spec}`"),
                1 => matches[0].clone(),
                count => bail!("`{spec}` starts {count} run ids or roots; give more of it"),
            }
        };
        let text = fs::read_to_string(self.run_path(&run_id)?)
            .with_context(|| format!("run {run_id} is not in the store"))?;
        let manifest: RunManifest = serde_json::from_str(&text).map_err(|error| {
            fault::integrity(format!("run {run_id} is not valid JSON: {error}"))
        })?;
        // The file name is how the run was found; the content must be that run.
        if manifest.run_id != run_id {
            return Err(fault::integrity(format!(
                "the file for run {run_id} holds run {}",
                manifest.run_id
            )));
        }
        Ok(manifest)
    }
}

/// Shortest run selector accepted besides `latest` and a full id. A shorter
/// one would match by accident, and keep matching a different run as more
/// runs are recorded.
pub const MIN_RUN_PREFIX: usize = 4;

/// The shape of id flot gives a run: a 13-digit millisecond stamp, `-`, and
/// the first 12 hex digits of its root.
fn is_run_id(id: &str) -> bool {
    id.split_once('-').is_some_and(|(stamp, root)| {
        stamp.len() == 13
            && stamp.chars().all(|c| c.is_ascii_digit())
            && root.len() == 12
            && root.chars().all(|c| c.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests;
