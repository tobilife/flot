//! Garbage collection: dropping the receipts, objects and cache entries no
//! recorded run references.

use std::collections::BTreeSet;
use std::fs;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::cache::CacheEntry;
use super::Store;

impl Store {
    /// Receipts and objects reachable from recorded runs.
    fn reachable(&self) -> Result<Reachable> {
        let mut reachable = Reachable::default();
        // What an unreadable record references is unknown, so collecting
        // anything could destroy evidence it needs. Stop instead.
        let unknown = |what: String| {
            move |error: anyhow::Error| {
                error.context(format!(
                    "gc stopped: {what} cannot be read, so what it references is unknown; repair or remove it first"
                ))
            }
        };
        for run_id in self.list_runs()? {
            let manifest = self
                .resolve_run(&run_id)
                .map_err(unknown(format!("run {run_id}")))?;
            for (_, receipt_id, observed_digest) in manifest.evidence() {
                reachable.receipts.insert(receipt_id.to_string());
                reachable.objects.insert(observed_digest.to_string());
                if !self.receipt_path(receipt_id)?.exists() {
                    continue;
                }
                let receipt = self
                    .load_receipt(receipt_id)
                    .map_err(unknown(format!("receipt {receipt_id}")))?;
                reachable
                    .objects
                    .extend(receipt.objects().into_iter().map(str::to_string));
            }
        }
        Ok(reachable)
    }

    /// Drop receipts, objects and cache entries that no recorded run references.
    ///
    /// Runs are the roots: deleting a run manifest is what makes its evidence
    /// collectable. Nothing reachable is ever removed, so a surviving run stays
    /// fully auditable after a collection.
    pub fn gc(&self, dry_run: bool) -> Result<GcReport> {
        if !self.root.join("FORMAT").exists() {
            return Ok(GcReport {
                dry_run,
                ..GcReport::default()
            });
        }
        let _lock = self.lock(true)?;
        let reachable = self.reachable()?;
        let runs: BTreeSet<String> = self.list_runs()?.into_iter().collect();
        let mut report = GcReport {
            dry_run,
            ..GcReport::default()
        };

        for (path, digest) in self.entries("receipts", ".json")? {
            if reachable.receipts.contains(&digest) {
                report.kept_receipts += 1;
            } else {
                report.removed_receipts += 1;
                if !dry_run {
                    fs::remove_file(&path)?;
                }
            }
        }

        let objects_root = self.root.join("objects");
        if objects_root.exists() {
            for shard in fs::read_dir(&objects_root)? {
                let shard = shard?;
                if !shard.path().is_dir() {
                    continue;
                }
                for entry in fs::read_dir(shard.path())? {
                    let path = entry?.path();
                    let digest = format!(
                        "sha256:{}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                    if reachable.objects.contains(&digest) {
                        report.kept_objects += 1;
                    } else {
                        report.removed_objects += 1;
                        if !dry_run {
                            fs::remove_file(&path)?;
                        }
                    }
                }
            }
        }

        for (path, _) in self.entries("cache", ".json")? {
            let still_usable = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<CacheEntry>(&text).ok())
                .map(|entry| reachable.receipts.contains(&entry.receipt_id))
                .unwrap_or(false);
            if !still_usable {
                report.removed_cache += 1;
                if !dry_run {
                    fs::remove_file(&path)?;
                }
            }
        }

        // Records that only describe a run: gone with the run.
        for dir in ["attestations", "imported"] {
            let path = self.root.join(dir);
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let run_id = name.strip_suffix(".json").unwrap_or(&name);
                if !runs.contains(run_id) && !dry_run {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        // No run holds the lock, so nothing in tmp/ is in use, nor any
        // build a proof made in a copy of the work, which the next one makes
        // again.
        if !dry_run {
            let builds = self.root.join("builds");
            if builds.exists() {
                fs::remove_dir_all(&builds)
                    .with_context(|| format!("cannot remove {}", builds.display()))?;
            }
            if let Ok(entries) = fs::read_dir(self.root.join("tmp")) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let _ = if path.is_dir() {
                        fs::remove_dir_all(&path)
                    } else {
                        fs::remove_file(&path)
                    };
                }
            }
        }

        Ok(report)
    }
}

#[derive(Debug, Default)]
struct Reachable {
    receipts: BTreeSet<String>,
    objects: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GcReport {
    pub dry_run: bool,
    pub kept_receipts: usize,
    pub removed_receipts: usize,
    pub kept_objects: usize,
    pub removed_objects: usize,
    pub removed_cache: usize,
}

#[cfg(test)]
mod tests;
