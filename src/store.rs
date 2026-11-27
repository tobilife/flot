//! The `.flot` store: content-addressed objects, receipts, the step-key cache
//! run manifests, the attestations over them, and the owner's trust list.
//!
//! Nothing in here is a database. Evidence (objects, receipts, observations,
//! run manifests, attestations) authenticates itself: objects and receipts are
//! named by digests of their content, and a manifest is checked against its
//! receipts. That part can be copied, committed or shipped and still be
//! audited anywhere.
//!
//! Two parts are decisions, not evidence: the step-key cache (which receipts
//! may stand in for running a step) and the trust list (whose signatures are
//! believed). Both are sealed with a secret that never leaves this host, so a
//! copied, committed or downloaded store cannot carry them in. A record whose
//! seal does not verify is ignored (cache) or refused (trust list).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::attest::Attestation;
use crate::core::digest;
use crate::core::fault;
use crate::core::receipt::{Observed, Receipt, Refusal, RunManifest};

pub const STORE_DIR: &str = ".flot";

/// What the guard refused, one JSON line each, until a run records it.
const REFUSALS: &str = "refusals.jsonl";

/// Layout and record format of a store. A store written in another format is
/// refused whole, with how to start over, rather than read half-right: flot
/// does not migrate evidence, because a migrated receipt is not what was
/// observed.
pub const STORE_FORMAT: u32 = 3;

/// A secret that never leaves this host. It seals the store's decisions
/// (cache entries, the trust list) so that only this host's flot can write
/// records it will act on.
#[derive(Clone)]
pub struct HostSecret([u8; 32]);

impl HostSecret {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        HostSecret(bytes)
    }

    fn seal(&self, domain: &str, value: &serde_json::Value) -> String {
        let message = digest::canonical_json(&serde_json::json!({
            "domain": domain,
            "value": value,
        }));
        digest::mac(&self.0, message.as_bytes())
    }
}

impl std::fmt::Debug for HostSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostSecret(..)")
    }
}

/// Which receipt answers for a step key, and on whose word.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    step_key: String,
    receipt_id: String,
    /// Signer keys of the bundle this receipt was imported from. Empty when
    /// this host executed the step. An imported entry is honoured only while
    /// one of these signers is still trusted.
    #[serde(default)]
    vouched_by: Vec<String>,
    seal: String,
}

impl CacheEntry {
    fn body(&self) -> serde_json::Value {
        serde_json::json!({
            "step_key": self.step_key,
            "receipt_id": self.receipt_id,
            "vouched_by": self.vouched_by,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrustFile {
    keys: BTreeMap<String, String>,
    seal: String,
}

/// A cache hit: the receipt, and the signers an imported one depends on.
#[derive(Debug, Clone)]
pub struct Recalled {
    pub receipt: Receipt,
    pub vouched_by: Vec<String>,
}

/// Held while a run or an import writes to the store, and exclusively by gc,
/// so a collection never removes evidence a run has not recorded yet.
pub struct StoreLock {
    _file: fs::File,
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    host: HostSecret,
}

impl Store {
    /// Open the store under `flow_root/.flot`. Nothing is created until the
    /// first write, so reading, or refusing a bundle, leaves no trace.
    pub fn open(flow_root: &Path, host: HostSecret) -> Result<Self> {
        let root = flow_root.join(STORE_DIR);
        let format_path = root.join("FORMAT");
        match fs::read_to_string(&format_path) {
            Ok(text) if text.trim() == STORE_FORMAT.to_string() => {}
            Ok(text) => bail!(
                "{} was written in store format {} and this flot reads format {STORE_FORMAT}; move it aside to start a new store",
                root.display(),
                text.trim()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let holds_runs = fs::read_dir(root.join("runs"))
                    .map(|mut entries| entries.next().is_some())
                    .unwrap_or(false);
                if holds_runs {
                    bail!(
                        "{} was written by an older flot (no store format); move it aside to start a new store",
                        root.display()
                    );
                }
            }
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", format_path.display()))
            }
        }
        Ok(Store { root, host })
    }

    /// Create the store on disk if it is not there yet.
    pub fn ensure(&self) -> Result<()> {
        let format_path = self.root.join("FORMAT");
        if format_path.exists() {
            return Ok(());
        }
        fs::create_dir_all(&self.root)
            .with_context(|| format!("cannot create {}", self.root.display()))?;
        // The store is this host's evidence, shared through bundles; a
        // committed copy would only be refused elsewhere. Keep it out of git
        // without asking the user to edit their own ignore file.
        write_atomic(
            &self.root.join(".gitignore"),
            b"# Created by flot. The store is local; share runs with `flot bundle`.\n*\n",
        )?;
        write_atomic(&format_path, STORE_FORMAT.to_string().as_bytes())
    }

    /// Take the store lock: shared for writers that only add (runs, imports),
    /// exclusive for gc, which fails at once rather than wait for a run.
    pub fn lock(&self, exclusive: bool) -> Result<StoreLock> {
        self.ensure()?;
        let path = self.root.join("lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let operation = if exclusive {
                libc::LOCK_EX | libc::LOCK_NB
            } else {
                libc::LOCK_SH
            };
            if unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    return Err(fault::environment(
                        "the store is in use by a running flot; try again when it finishes",
                    ));
                }
                return Err(error).context("cannot lock the store");
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, HANDLE};
            use windows_sys::Win32::Storage::FileSystem::{
                LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
            };
            let flags = if exclusive {
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY
            } else {
                0
            };
            let mut overlapped = unsafe { std::mem::zeroed() };
            // The whole file; the lock goes when the handle closes.
            let locked = unsafe {
                LockFileEx(
                    file.as_raw_handle() as HANDLE,
                    flags,
                    0,
                    u32::MAX,
                    u32::MAX,
                    &mut overlapped,
                )
            };
            if locked == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
                    return Err(fault::environment(
                        "the store is in use by a running flot; try again when it finishes",
                    ));
                }
                return Err(error).context("cannot lock the store");
            }
        }
        #[cfg(not(any(unix, windows)))]
        let _ = exclusive;
        Ok(StoreLock { _file: file })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn hex(digest: &str) -> Result<&str> {
        let hex = digest
            .strip_prefix("sha256:")
            .with_context(|| format!("unsupported digest form `{digest}`"))?;
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("digest `{digest}` is not a sha256 hex digest");
        }
        Ok(hex)
    }

    fn object_path(&self, object: &str) -> Result<PathBuf> {
        let hex = Self::hex(object)?;
        Ok(self.root.join("objects").join(&hex[..2]).join(hex))
    }

    /// Store bytes under their digest. Writing the same bytes twice is a no-op.
    /// An existing object is re-read first: one that no longer matches its
    /// name (a torn write, bit rot, tampering) is replaced, not trusted.
    pub fn put_bytes(&self, data: &[u8]) -> Result<String> {
        let object = digest::bytes(data);
        let path = self.object_path(&object)?;
        if fs::read(&path).is_ok_and(|existing| digest::bytes(&existing) == object) {
            return Ok(object);
        }
        self.ensure()?;
        fs::create_dir_all(path.parent().expect("object path has a parent"))?;
        write_atomic(&path, data)?;
        Ok(object)
    }

    /// Keep one run's observation of a step, named by its digest.
    pub fn put_observation(&self, observed: &Observed) -> Result<String> {
        self.put_bytes(digest::canonical_json(observed).as_bytes())
    }

    pub fn load_observation(&self, object: &str) -> Result<Observed> {
        serde_json::from_slice(&self.read_bytes(object)?).map_err(|error| {
            fault::integrity(format!("object {object} is not an observation: {error}"))
        })
    }

    pub fn has_object(&self, object: &str) -> bool {
        self.object_path(object)
            .map(|path| path.exists())
            .unwrap_or(false)
    }

    pub fn read_bytes(&self, object: &str) -> Result<Vec<u8>> {
        let path = self.object_path(object)?;
        let data = fs::read(&path)
            .with_context(|| format!("object {object} is missing from the store"))?;
        let actual = digest::bytes(&data);
        if actual != object {
            return Err(fault::integrity(format!(
                "object {object} does not match its content (found {actual})"
            )));
        }
        Ok(data)
    }

    fn receipt_path(&self, receipt_id: &str) -> Result<PathBuf> {
        Ok(self
            .root
            .join("receipts")
            .join(format!("{}.json", Self::hex(receipt_id)?)))
    }

    /// Resolve a full receipt id or an unambiguous hex prefix.
    pub fn find_receipt(&self, spec: &str) -> Result<Receipt> {
        if spec.starts_with("sha256:") {
            return self.load_receipt(spec);
        }
        let mut matches = Vec::new();
        for (_, receipt_id) in self.entries("receipts", ".json")? {
            if receipt_id["sha256:".len()..].starts_with(spec) {
                matches.push(receipt_id);
            }
        }
        match matches.len() {
            0 => bail!("no receipt matches `{spec}`"),
            1 => self.load_receipt(&matches[0]),
            count => bail!("`{spec}` matches {count} receipts; use a longer prefix"),
        }
    }

    pub fn put_receipt(&self, receipt: &Receipt) -> Result<()> {
        if !receipt.is_authentic() {
            bail!(
                "refusing to store receipt {}: its id does not match its body",
                receipt.receipt_id
            );
        }
        let path = self.receipt_path(&receipt.receipt_id)?;
        self.ensure()?;
        write_atomic(&path, serde_json::to_string_pretty(receipt)?.as_bytes())
    }

    pub fn load_receipt(&self, receipt_id: &str) -> Result<Receipt> {
        let path = self.receipt_path(receipt_id)?;
        let text = fs::read_to_string(&path)
            .with_context(|| format!("receipt {receipt_id} is not in the store"))?;
        let receipt: Receipt = serde_json::from_str(&text).map_err(|error| {
            fault::integrity(format!("receipt {receipt_id} is not valid JSON: {error}"))
        })?;
        // The file name is the claim; the content must be the receipt it names.
        if receipt.receipt_id != receipt_id {
            return Err(fault::integrity(format!(
                "the file for receipt {receipt_id} holds receipt {}",
                receipt.receipt_id
            )));
        }
        Ok(receipt)
    }

    fn cache_path(&self, step_key: &str) -> Result<PathBuf> {
        Ok(self
            .root
            .join("cache")
            .join(format!("{}.json", Self::hex(step_key)?)))
    }

    /// Let `receipt_id` answer for `step_key`. `vouched_by` names the signers
    /// of the bundle it came from; empty when this host ran the step.
    pub fn remember(&self, step_key: &str, receipt_id: &str, vouched_by: &[String]) -> Result<()> {
        let mut entry = CacheEntry {
            step_key: step_key.to_string(),
            receipt_id: receipt_id.to_string(),
            vouched_by: vouched_by.to_vec(),
            seal: String::new(),
        };
        entry.seal = self.host.seal("flot-cache-entry-v1", &entry.body());
        self.ensure()?;
        write_atomic(
            &self.cache_path(step_key)?,
            serde_json::to_string_pretty(&entry)?.as_bytes(),
        )
    }

    /// Look up a reusable receipt for a step key.
    ///
    /// A cache hit must be provable: the receipt has to parse, authenticate
    /// against its own id, carry the requested step key, record a settled
    /// success (see `Receipt::is_reusable`), and still have every object it
    /// references present and intact. The entry itself must carry this host's
    /// seal. Anything else is treated as a miss, because a cache that can lie
    /// is worse than no cache, and one that replays a transient failure turns
    /// a flaky step into a permanent one.
    pub fn recall(&self, step_key: &str) -> Result<Option<Recalled>> {
        let path = self.cache_path(step_key)?;
        let Ok(text) = fs::read_to_string(&path) else {
            return Ok(None);
        };
        let Ok(entry) = serde_json::from_str::<CacheEntry>(&text) else {
            return Ok(None);
        };
        if entry.step_key != step_key
            || entry.seal != self.host.seal("flot-cache-entry-v1", &entry.body())
        {
            return Ok(None);
        }
        let Ok(receipt) = self.load_receipt(&entry.receipt_id) else {
            return Ok(None);
        };
        if !receipt.is_authentic()
            || receipt.identity.step_key != step_key
            || !receipt.is_reusable()
        {
            return Ok(None);
        }
        if !receipt
            .objects()
            .into_iter()
            .all(|object| self.read_bytes(object).is_ok())
        {
            return Ok(None);
        }
        Ok(Some(Recalled {
            receipt,
            vouched_by: entry.vouched_by,
        }))
    }

    /// Run ids name files, so anything beyond the alphabet flot itself
    /// produces is refused before it reaches a path.
    fn run_file(&self, dir: &str, run_id: &str) -> Result<PathBuf> {
        if !is_run_id(run_id) {
            bail!("`{run_id}` is not a run id");
        }
        Ok(self.root.join(dir).join(format!("{run_id}.json")))
    }

    fn run_path(&self, run_id: &str) -> Result<PathBuf> {
        self.run_file("runs", run_id)
    }

    fn attestation_path(&self, run_id: &str) -> Result<PathBuf> {
        self.run_file("attestations", run_id)
    }

    /// Attestations recorded for a run. None recorded is an empty list.
    pub fn attestations(&self, run_id: &str) -> Result<Vec<Attestation>> {
        let path = self.attestation_path(run_id)?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&path)?;
        serde_json::from_str(&text).map_err(|error| {
            fault::integrity(format!(
                "attestations for run {run_id} are corrupt: {error}"
            ))
        })
    }

    /// Record attestations for a run, keeping one per signer key.
    pub fn add_attestations(&self, run_id: &str, attestations: &[Attestation]) -> Result<()> {
        let mut known = self.attestations(run_id)?;
        self.ensure()?;
        for attestation in attestations {
            known.retain(|existing| existing.key != attestation.key);
            known.push(attestation.clone());
        }
        known.sort_by(|a, b| a.key.cmp(&b.key));
        write_atomic(
            &self.attestation_path(run_id)?,
            serde_json::to_string_pretty(&known)?.as_bytes(),
        )
    }

    /// Signers the store's owner has chosen to believe: canonical key -> name.
    /// A trust list this host did not write is refused, not believed: one that
    /// arrived with a copied or committed store would decide whose bundles
    /// may stand in for local work.
    pub fn trusted_keys(&self) -> Result<BTreeMap<String, String>> {
        let path = self.root.join("trust.json");
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        let file: TrustFile = serde_json::from_str(&fs::read_to_string(&path)?)
            .map_err(|error| fault::integrity(format!("the trust list is corrupt: {error}")))?;
        if file.seal
            != self
                .host
                .seal("flot-trust-list-v1", &serde_json::json!(file.keys))
        {
            return Err(fault::integrity(format!(
                "the trust list {} was not written by this host's flot; delete it and add the signers you trust again",
                path.display()
            )));
        }
        Ok(file.keys)
    }

    pub fn save_trusted_keys(&self, keys: &BTreeMap<String, String>) -> Result<()> {
        let file = TrustFile {
            keys: keys.clone(),
            seal: self
                .host
                .seal("flot-trust-list-v1", &serde_json::json!(keys)),
        };
        self.ensure()?;
        write_atomic(
            &self.root.join("trust.json"),
            serde_json::to_string_pretty(&file)?.as_bytes(),
        )
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

    /// Note an edit the guard refused, for the next run to record. Only in a
    /// store that is already there: a refusal does not create one.
    pub fn note_refusal(&self, refusal: &Refusal) -> Result<()> {
        if !self.root.join("FORMAT").is_file() {
            return Ok(());
        }
        let mut line = serde_json::to_string(refusal)?;
        line.push('\n');
        let path = self.root.join(REFUSALS);
        // One write of one line: notes from hooks running at once do not
        // interleave.
        std::io::Write::write_all(
            &mut fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("cannot open {}", path.display()))?,
            line.as_bytes(),
        )
        .with_context(|| format!("cannot write {}", path.display()))
    }

    /// Refusals noted after `after_ms` and up to `until_ms`, oldest first. A
    /// line that does not parse, such as one cut short, is skipped.
    pub fn refusals(&self, after_ms: u64, until_ms: u64) -> Result<Vec<Refusal>> {
        let path = self.root.join(REFUSALS);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()))
            }
        };
        let mut found: Vec<Refusal> = text
            .lines()
            .filter_map(|line| serde_json::from_str::<Refusal>(line).ok())
            .filter(|refusal| refusal.at_ms > after_ms && refusal.at_ms <= until_ms)
            .collect();
        found.sort_by_key(|refusal| refusal.at_ms);
        Ok(found)
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
        // No run holds the lock, so nothing in tmp/ is in use.
        if !dry_run {
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

    /// `(path, digest)` pairs for digest-named files in one store directory.
    fn entries(&self, directory: &str, suffix: &str) -> Result<Vec<(PathBuf, String)>> {
        let dir = self.root.join(directory);
        let mut found = Vec::new();
        if !dir.exists() {
            return Ok(found);
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if let Some(hex) = name.strip_suffix(suffix) {
                found.push((path, format!("sha256:{hex}")));
            }
        }
        Ok(found)
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

/// Monotonic suffix source so two concurrent writers of the same digest cannot
/// share a temporary file and tear each other's bytes.
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!(
        "tmp{}-{}",
        std::process::id(),
        WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    {
        use std::io::Write;
        let mut file = fs::File::create(&temporary)
            .with_context(|| format!("cannot write {}", temporary.display()))?;
        file.write_all(data)
            .with_context(|| format!("cannot write {}", temporary.display()))?;
        // On disk before it has a name, so a crash never leaves a named file
        // with torn content.
        file.sync_all()
            .with_context(|| format!("cannot write {}", temporary.display()))?;
    }
    fs::rename(&temporary, path).with_context(|| format!("cannot place {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::receipt::{Observed, ReceiptIdentity, StepStatus, ToolIdentity};

    fn test_host() -> HostSecret {
        HostSecret::from_bytes([7; 32])
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), test_host()).unwrap();
        store.ensure().unwrap();
        (dir, store)
    }

    #[test]
    fn a_store_in_another_format_is_refused_not_misread() {
        let (dir, store) = store();
        drop(store);
        assert!(
            Store::open(dir.path(), test_host()).is_ok(),
            "reopening is fine"
        );
        std::fs::write(dir.path().join(".flot/FORMAT"), "1").unwrap();
        let error = Store::open(dir.path(), test_host())
            .unwrap_err()
            .to_string();
        assert!(error.contains("store format 1"), "{error}");

        let old = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(old.path().join(".flot/runs")).unwrap();
        std::fs::write(old.path().join(".flot/runs/1-a.json"), "{}").unwrap();
        let error = Store::open(old.path(), test_host())
            .unwrap_err()
            .to_string();
        assert!(error.contains("older flot"), "{error}");
    }

    #[test]
    fn a_run_id_can_never_name_a_file_outside_the_store() {
        let (_dir, store) = store();
        for id in ["../escape", "a/b", "", "..", "x.json"] {
            assert!(store.attestations(id).is_err(), "{id}");
            assert!(store.add_attestations(id, &[]).is_err(), "{id}");
        }
        assert!(store.attestations("1795496079152-c5fe4934ed4a").is_ok());
    }

    fn receipt(step_key: &str, stdout: &[u8]) -> Receipt {
        Receipt::seal(
            ReceiptIdentity {
                flow: "demo".into(),
                step_id: "a".into(),
                step_key: step_key.into(),
                command: "echo hi".into(),
                status: StepStatus::Ok,
                exit_code: Some(0),
                stdout_digest: digest::bytes(stdout),
                stderr_digest: digest::bytes(b""),
                inputs: vec![],
                verify_inputs: vec![],
                outputs: vec![],
                changed: Default::default(),
                deps: vec![],
                claims: vec![],
                limitations: vec![],
                tool: ToolIdentity::default(),
                candidate: None,
                sandbox: None,
            },
            Observed::default(),
        )
    }

    #[test]
    fn objects_round_trip_under_their_digest() {
        let (_dir, store) = store();
        let object = store.put_bytes(b"hello").unwrap();
        assert_eq!(object, digest::bytes(b"hello"));
        assert_eq!(store.read_bytes(&object).unwrap(), b"hello");
        assert!(store.has_object(&object));
    }

    #[test]
    fn storing_the_same_bytes_twice_is_idempotent() {
        let (_dir, store) = store();
        let first = store.put_bytes(b"same").unwrap();
        let second = store.put_bytes(b"same").unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_corrupted_object_file_is_reported_not_returned() {
        let (_dir, store) = store();
        let object = store.put_bytes(b"trusted").unwrap();
        fs::write(store.object_path(&object).unwrap(), b"swapped").unwrap();
        let err = store.read_bytes(&object).unwrap_err();
        assert!(
            err.to_string().contains("does not match its content"),
            "{err}"
        );
    }

    #[test]
    fn unknown_digest_forms_are_rejected() {
        let (_dir, store) = store();
        assert!(store.read_bytes("md5:abc").is_err());
        assert!(store.read_bytes("sha256:short").is_err());
    }

    #[test]
    fn cache_hit_requires_authentic_receipt_and_present_outputs() {
        let (_dir, store) = store();
        let key = digest::bytes(b"step-key");
        let receipt = receipt(&key, b"out");
        store.put_bytes(b"out").unwrap();
        store.put_bytes(b"").unwrap();
        store.put_receipt(&receipt).unwrap();
        store.remember(&key, &receipt.receipt_id, &[]).unwrap();

        let hit = store.recall(&key).unwrap().expect("cache hit");
        assert_eq!(hit.receipt.receipt_id, receipt.receipt_id);
        assert!(hit.vouched_by.is_empty());
    }

    #[test]
    fn cache_misses_when_the_output_object_was_pruned() {
        let (_dir, store) = store();
        let key = digest::bytes(b"step-key");
        let receipt = receipt(&key, b"vanished");
        store.put_receipt(&receipt).unwrap();
        store.remember(&key, &receipt.receipt_id, &[]).unwrap();
        assert!(store.recall(&key).unwrap().is_none());
    }

    #[test]
    fn a_failed_receipt_is_never_a_cache_hit() {
        let (_dir, store) = store();
        let key = digest::bytes(b"step-key");
        let mut body = receipt(&key, b"out").identity;
        body.status = StepStatus::Failed;
        body.exit_code = Some(1);
        let failed = Receipt::seal(body, Observed::default());
        store.put_bytes(b"out").unwrap();
        store.put_bytes(b"").unwrap();
        store.put_receipt(&failed).unwrap();
        store.remember(&key, &failed.receipt_id, &[]).unwrap();
        assert!(store.recall(&key).unwrap().is_none());
    }

    #[test]
    fn cache_misses_on_an_unknown_key() {
        let (_dir, store) = store();
        assert!(store
            .recall(&digest::bytes(b"never-seen"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn tampered_receipts_are_refused_at_write_time() {
        let (_dir, store) = store();
        let mut tampered = receipt(&digest::bytes(b"k"), b"out");
        tampered.identity.status = StepStatus::Failed;
        let err = store.put_receipt(&tampered).unwrap_err();
        assert!(err.to_string().contains("does not match its body"), "{err}");
    }

    #[test]
    fn gc_removes_evidence_that_no_run_references() {
        let (_dir, store) = store();
        let orphan = store.put_bytes(b"orphan").unwrap();
        let receipt = receipt(&digest::bytes(b"k"), b"orphan");
        store.put_receipt(&receipt).unwrap();
        store
            .remember(&digest::bytes(b"k"), &receipt.receipt_id, &[])
            .unwrap();

        let report = store.gc(false).unwrap();
        assert_eq!(report.removed_receipts, 1);
        assert_eq!(report.removed_objects, 1);
        assert_eq!(report.removed_cache, 1);
        assert!(!store.has_object(&orphan));
        assert!(store.recall(&digest::bytes(b"k")).unwrap().is_none());
    }

    #[test]
    fn gc_dry_run_reports_without_deleting() {
        let (_dir, store) = store();
        let orphan = store.put_bytes(b"orphan").unwrap();
        let report = store.gc(true).unwrap();
        assert!(report.dry_run);
        assert_eq!(report.removed_objects, 1);
        assert!(store.has_object(&orphan), "a dry run must not delete");
    }

    #[test]
    fn a_cache_entry_or_trust_list_from_another_host_is_not_believed() {
        let (dir, store) = store();
        let key = digest::bytes(b"step-key");
        let receipt = receipt(&key, b"out");
        store.put_bytes(b"out").unwrap();
        store.put_bytes(b"").unwrap();
        store.put_receipt(&receipt).unwrap();
        store.remember(&key, &receipt.receipt_id, &[]).unwrap();
        store
            .save_trusted_keys(&BTreeMap::from([("ed25519:00".into(), "alice".into())]))
            .unwrap();

        // The same files, read by a host with another secret.
        let elsewhere = Store::open(dir.path(), HostSecret::from_bytes([8; 32])).unwrap();
        assert!(elsewhere.recall(&key).unwrap().is_none());
        let error = elsewhere.trusted_keys().unwrap_err().to_string();
        assert!(error.contains("not written by this host"), "{error}");
        assert!(store.recall(&key).unwrap().is_some());
        assert_eq!(store.trusted_keys().unwrap().len(), 1);
    }

    /// The exact bytes of a sealed cache entry: its fields in order and the
    /// seal this host's secret puts on them. These change only with the
    /// store format, or every cache entry a host wrote stops being believed.
    #[test]
    fn a_sealed_cache_entry_has_exactly_these_bytes() {
        let (_dir, store) = store();
        let entry = |step_key: &[u8], vouched_by: &[String]| {
            let step_key = digest::bytes(step_key);
            store
                .remember(&step_key, &digest::bytes(b"receipt"), vouched_by)
                .unwrap();
            fs::read_to_string(store.cache_path(&step_key).unwrap()).unwrap()
        };
        assert_eq!(entry(b"ran here", &[]), GOLDEN_LOCAL_ENTRY);
        assert_eq!(
            entry(b"imported", &["ed25519:00".into()]),
            GOLDEN_IMPORTED_ENTRY
        );
    }

    const GOLDEN_LOCAL_ENTRY: &str = r#"{
  "step_key": "sha256:12ae9ef07cb551a948d684543b148f8c3b77f08c675ef8bafe37db723d8d784d",
  "receipt_id": "sha256:6f32860910ca0fb2a20c7fda143666b09dbf8db5238195c90a586fb542ff0cad",
  "vouched_by": [],
  "seal": "7c5ebe018a021e44bc43feff6d3063313e4774b850095046a7075400b2496518"
}"#;

    const GOLDEN_IMPORTED_ENTRY: &str = r#"{
  "step_key": "sha256:5f54227b74fbba7743c47cd286b4873f2e17331518d56facfc03e34cde4a0950",
  "receipt_id": "sha256:6f32860910ca0fb2a20c7fda143666b09dbf8db5238195c90a586fb542ff0cad",
  "vouched_by": [
    "ed25519:00"
  ],
  "seal": "87b4571d2a397671d0f66d987f462a16d568236ad9d10ce756689d0328a7133c"
}"#;

    #[test]
    fn nothing_is_created_until_something_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), test_host()).unwrap();
        assert!(store.list_runs().unwrap().is_empty());
        assert!(store.recall(&digest::bytes(b"k")).unwrap().is_none());
        assert!(!dir.path().join(STORE_DIR).exists());
    }

    #[test]
    fn gc_waits_for_no_one_and_never_runs_under_a_writer() {
        let (_dir, store) = store();
        let _writing = store.lock(false).unwrap();
        let error = store.gc(false).unwrap_err().to_string();
        assert!(error.contains("in use by a running flot"), "{error}");
    }

    #[test]
    fn a_corrupt_object_is_replaced_when_written_again() {
        let (dir, store) = store();
        let object = store.put_bytes(b"real").unwrap();
        let hex = &object["sha256:".len()..];
        let path = dir.path().join(".flot/objects").join(&hex[..2]).join(hex);
        std::fs::write(&path, b"rot").unwrap();
        assert!(store.read_bytes(&object).is_err());
        store.put_bytes(b"real").unwrap();
        assert_eq!(store.read_bytes(&object).unwrap(), b"real");
    }
}
