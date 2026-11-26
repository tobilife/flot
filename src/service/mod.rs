//! The contract. Both adapters expose exactly these operations, so a person at
//! a terminal and an agent over MCP reach the same receipts, verdicts and
//! digests for the same request. Adapters only parse and render.

pub mod audit;
pub mod bundle;
pub mod doctor;
pub mod flaky;
mod guard;
pub mod keys;
pub mod search;
pub mod share;

pub use guard::{answer, judge, tilde, Answer, Call, Guarded, Judged, Refused, Shell};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::core::compare::{self, RunDiff};
use crate::core::flow::Flow;
use crate::core::receipt::{ClaimCounts, ClaimRecord, Receipt, Refusal, RunManifest, Verdict};
use crate::engine::baseline::Baseline;
use crate::engine::exec;
use crate::engine::git::Git;
use crate::engine::runner::Runner;
// The request and report types are part of this contract; adapters take them
// from here and never reach into the engine.
pub use crate::core::fault::FaultKind;
use crate::core::{attest, fault};
pub use crate::engine::exec::{install_interrupt_handlers, interrupted, StopSignal};

/// What kind of failure `error` is. An error that names its kind is taken at
/// its word; otherwise an IO failure underneath is the environment's (a
/// missing file is a request for something that is not there, and unreadable
/// bytes are corrupt evidence), and anything else is an invalid request.
pub fn fault_kind(error: &anyhow::Error) -> FaultKind {
    for cause in error.chain() {
        if let Some(fault) = cause.downcast_ref::<crate::core::fault::Fault>() {
            return fault.kind;
        }
    }
    for cause in error.chain() {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return match io.kind() {
                std::io::ErrorKind::NotFound => FaultKind::Invalid,
                std::io::ErrorKind::InvalidData => FaultKind::Integrity,
                _ => FaultKind::Environment,
            };
        }
    }
    FaultKind::Invalid
}
pub use crate::engine::runner::{PlanEntry, RunOptions, RunReport, StepReport};
use crate::service::audit::Audit;
use crate::service::bundle::{ExportReport, ImportPolicy, ImportReport};
use crate::store::{GcReport, Store};

pub const DEFAULT_FLOW_FILE: &str = "flot.toml";

/// How a run leaves this host, by `bundle` or `push`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sharing {
    /// Attest the run with the host key first.
    pub sign: bool,
    /// Share it even when it carries credential-shaped tokens.
    pub allow_secrets: bool,
}

/// The bundle of a run about to leave this host. A run that carries what
/// looks like a credential is refused before anything is signed or written:
/// receipts are content-addressed, so a token cannot be masked out of the
/// evidence, only kept out of the run.
fn shareable(store: &Store, manifest: &RunManifest, share: Sharing) -> Result<bundle::Bundle> {
    let key = if share.sign {
        let key = keys::load(&keys::default_path()?)?;
        signable(store, manifest)?;
        Some(key)
    } else {
        None
    };
    let unsigned = bundle::export(store, manifest, None)?;
    if !share.allow_secrets {
        let found = bundle::exposures(&unsigned)?;
        if !found.is_empty() {
            let list: Vec<String> = found
                .iter()
                .map(|e| format!("  - {}: {} ({})", e.place, e.kind, e.shown))
                .collect();
            bail!(
                "refusing to share run {}: it carries what looks like credentials, and nothing left this host:\n{}\nkeep them out of the step's output and run it again, or pass --allow-secrets if they are not credentials",
                manifest.run_id,
                list.join("\n")
            );
        }
    }
    let Some(key) = key else {
        return Ok(unsigned);
    };
    attest_recorded(store, manifest, &key)?;
    bundle::export(store, manifest, None)
}

/// The flow `flot init` writes, with `{proofs_from}` replaced by
/// [`starter_proofs_from`].
const STARTER_FLOW: &str = r#"# flot flow. Every step records a receipt; every claim needs a proof command.
[flow]
name = "starter"
description = "Replace these steps with your own work."
{proofs_from}budget = { timeout_secs = 900, max_inline_bytes = 4096 }

[[step]]
id = "inspect"
run = "ls -a"
# Declared inputs make a cache hit trustworthy: change them and the step re-runs.
inputs = ["flot.toml"]
claims = ["the flow file is present"]
verify = "test -f flot.toml"

[[step]]
id = "report"
run = "echo 'replace me with real work'"
needs = ["inspect"]
# No claims: this step's success proves nothing, and flot will say so.
"#;

/// The `proofs_from` of a starter flow in `root`: the remote's default
/// branch when the repository names one (`origin/main` after a clone), a
/// revision an agent working here does not control; else a comment saying
/// what to set and why.
fn starter_proofs_from(root: &Path) -> String {
    let remote = Git::open(root)
        .and_then(|git| {
            git.run(
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
                None,
            )
        })
        .ok()
        .filter(|rev| !rev.is_empty() && !rev.starts_with('-'));
    match remote {
        Some(rev) => format!(
            "# Every proof must match {rev}, which an agent working here does not\n# control: a proof changed along with the work stays unverified.\nproofs_from = {}\n",
            toml::Value::String(rev.clone())
        ),
        None => "# Without proofs_from, a proof edited along with the work it judges passes.\n# Name a revision an agent working here does not control, such as the\n# remote's default branch:\n# proofs_from = \"origin/main\"\n"
            .to_string(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub flow: String,
    pub flow_digest: String,
    pub steps: Vec<PlanEntry>,
}

/// A recorded run with the claims each step made, read from its receipts.
#[derive(Debug, Clone, Serialize)]
pub struct RunDetail {
    #[serde(flatten)]
    pub manifest: RunManifest,
    /// The run arrived in a bundle rather than ran here.
    pub imported: bool,
    /// Claims by step id. A step whose receipt is unreadable or does not
    /// authenticate is absent here; `verify` reports why.
    pub step_claims: BTreeMap<String, Vec<ClaimRecord>>,
    /// Steps the run reused from imported runs, with who vouched for each,
    /// as [`audit::vouched`] judges them now.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub vouched: Vec<attest::VouchedStep>,
}

/// Recorded runs, newest first, and the ones that could not be read.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RunList {
    pub runs: Vec<RunSummary>,
    pub unreadable: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub verdict: Verdict,
    pub root_digest: String,
    pub claims: ClaimCounts,
    pub steps: usize,
    /// The run arrived in a bundle rather than ran here.
    pub imported: bool,
}

/// Which execution of a step [`Workspace::step_output`] reads.
#[derive(Debug, Clone, Default)]
pub struct OutputOf {
    /// A candidate by name; by default, the one the run kept.
    pub candidate: Option<String>,
    /// An attempt, numbered from 1; by default, the last.
    pub attempt: Option<usize>,
}

/// A stored object under a delivery budget. The complete object stays
/// retrievable; `recovery` says how when the view is cut.
#[derive(Debug, Clone, Serialize)]
pub struct ObjectView {
    pub digest: String,
    pub bytes: usize,
    pub truncated: bool,
    pub text: String,
    pub recovery: Option<String>,
}

/// A signing identity, without its secret.
#[derive(Debug, Clone, Serialize)]
pub struct KeyInfo {
    pub path: PathBuf,
    pub public_key: String,
    /// False when the key already existed and was only read.
    pub created: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignReport {
    pub run_id: String,
    pub root_digest: String,
    pub signer: String,
    /// False when this key had already signed the run.
    pub added: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrustEntry {
    pub name: String,
    pub key: String,
}

/// The host key was replaced; `handover` tells readers about the new one.
#[derive(Debug, Clone, Serialize)]
pub struct KeyRotation {
    pub path: PathBuf,
    pub old_key: String,
    pub new_key: String,
    /// The old secret, kept owner-only beside the new key.
    pub retired: PathBuf,
    /// The statement both keys signed, for everyone who trusts the old key.
    pub handover: PathBuf,
}

/// A trusted signer followed to their new key.
#[derive(Debug, Clone, Serialize)]
pub struct TrustRotation {
    pub name: String,
    pub from: String,
    pub to: String,
    /// False when the new key was already trusted under this name.
    pub added: bool,
    /// The old key is no longer trusted.
    pub revoked: bool,
}

/// One flow file and the `.flot` store beside it.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    flow_path: PathBuf,
}

impl Workspace {
    pub fn at(root: &Path, flow_path: &Path) -> Self {
        Workspace {
            root: root.to_path_buf(),
            flow_path: flow_path.to_path_buf(),
        }
    }

    /// The flow a command means. With no file named, the nearest `flot.toml`
    /// at or above `cwd`, so flot works from any subdirectory of a project;
    /// when there is none, `cwd` itself. A named file is exactly that file,
    /// relative to `cwd`, and it must exist. Either way the store sits beside
    /// the flow file, so one flow always has one store.
    pub fn locate(cwd: &Path, file: Option<&Path>) -> Result<Self> {
        match file {
            Some(file) => {
                let path = cwd.join(file);
                if !path.is_file() {
                    bail!("flow file {} does not exist", path.display());
                }
                Ok(Self::beside(&path))
            }
            None => {
                let root = find_flow_root(cwd, Path::new(DEFAULT_FLOW_FILE));
                Ok(Workspace::at(&root, &root.join(DEFAULT_FLOW_FILE)))
            }
        }
    }

    /// Where `flot init` writes: exactly the named file, or `flot.toml` in
    /// `cwd`. Never a flow file further up, which belongs to another project.
    pub fn for_init(cwd: &Path, file: Option<&Path>) -> Self {
        Self::beside(&cwd.join(file.unwrap_or(Path::new(DEFAULT_FLOW_FILE))))
    }

    fn beside(flow_path: &Path) -> Self {
        let root = flow_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"));
        Workspace::at(&root, flow_path)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn flow_path(&self) -> &Path {
        &self.flow_path
    }

    pub fn flow(&self) -> Result<Flow> {
        let text = std::fs::read_to_string(&self.flow_path)
            .with_context(|| format!("cannot read flow file {}", self.flow_path.display()))?;
        Flow::parse(&text)
    }

    fn store(&self) -> Result<Store> {
        Store::open(&self.root, keys::host_secret()?)
    }

    /// Note that the guard refused `tool` for `reasons`, for this flow's next
    /// run to record. Only where the store already is.
    pub fn note_refusal(&self, tool: &str, reasons: &[String]) -> Result<()> {
        if !self
            .root
            .join(crate::store::STORE_DIR)
            .join("FORMAT")
            .is_file()
        {
            return Ok(());
        }
        self.store()?.note_refusal(&Refusal {
            at_ms: crate::engine::exec::now_ms(),
            tool: tool.to_string(),
            reasons: reasons.to_vec(),
        })
    }

    /// Write the starter flow. Refuses to replace an existing file unless
    /// `force` is set.
    pub fn init(&self, force: bool) -> Result<PathBuf> {
        if self.flow_path.exists() && !force {
            bail!(
                "{} already exists; pass --force to overwrite",
                self.flow_path.display()
            );
        }
        let flow = STARTER_FLOW.replace("{proofs_from}", &starter_proofs_from(&self.root));
        std::fs::write(&self.flow_path, flow)
            .with_context(|| format!("cannot write {}", self.flow_path.display()))?;
        Ok(self.flow_path.clone())
    }

    pub fn plan(&self, options: &RunOptions) -> Result<Plan> {
        let flow = self.flow()?;
        let store = self.store()?;
        let baseline = self.baseline(Some(&flow), options.proofs_from.as_deref())?;
        let steps = Runner::new(&flow, &self.root, &store)
            .with_baseline(baseline.as_ref())
            .plan(options)?;
        Ok(Plan {
            flow_digest: flow.digest(),
            flow: flow.meta.name,
            steps,
        })
    }

    pub fn run(&self, options: &RunOptions) -> Result<RunReport> {
        let flow = self.flow()?;
        let store = self.store()?;
        let baseline = self.baseline(Some(&flow), options.proofs_from.as_deref())?;
        let mut report = Runner::new(&flow, &self.root, &store)
            .with_baseline(baseline.as_ref())
            .run(options)?;
        report.vouched = audit::vouched(&store, &report.manifest)?;
        Ok(report)
    }

    /// The revision proofs must match: `named`, else the committed flow's
    /// `proofs_from`, else the working copy's. The committed setting wins so
    /// that an edit to the working copy cannot drop it.
    fn baseline(&self, flow: Option<&Flow>, named: Option<&str>) -> Result<Option<Baseline>> {
        let file = self
            .flow_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| DEFAULT_FLOW_FILE.into());
        let committed = || -> Option<String> {
            let git = Git::open(&self.root).ok()?;
            let id = git
                .run(
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("HEAD:./{file}"),
                    ],
                    None,
                )
                .ok()?;
            let text = String::from_utf8(git.blob(&id, 1 << 20).ok()?).ok()?;
            Flow::parse(&text).ok()?.meta.proofs_from
        };
        let rev = match named {
            Some(rev) => Some(rev.to_string()),
            None => committed().or_else(|| flow.and_then(|flow| flow.meta.proofs_from.clone())),
        };
        rev.map(|rev| Baseline::load(&self.root, &file, &rev))
            .transpose()
    }

    /// A recorded run: `latest`, an exact id, or an unambiguous substring.
    pub fn show(&self, run: &str) -> Result<RunDetail> {
        let store = self.store()?;
        let manifest = store.resolve_run(run)?;
        let step_claims = manifest
            .steps
            .iter()
            .filter_map(|step| {
                store
                    .load_receipt(&step.receipt_id)
                    .ok()
                    .filter(Receipt::is_authentic)
                    .map(|receipt| (step.step_id.clone(), receipt.identity.claims))
            })
            .collect();
        let vouched = audit::vouched(&store, &manifest)?;
        Ok(RunDetail {
            imported: store.is_imported(&manifest.run_id),
            manifest,
            step_claims,
            vouched,
        })
    }

    /// Re-authenticate a recorded run and, when a revision is named here or
    /// set by `proofs_from`, hold its proofs to that revision.
    pub fn verify(&self, run: &str, proofs_from: Option<&str>) -> Result<Audit> {
        let store = self.store()?;
        let manifest = store.resolve_run(run)?;
        let mut audit = audit::audit(&store, &manifest)?;
        // The flow is optional here: a run can be verified without one.
        let flow = self.flow().ok();
        if let Some(baseline) = self.baseline(flow.as_ref(), proofs_from)? {
            audit.hold_to(audit::anchor(&store, &manifest, &baseline)?);
        }
        Ok(audit)
    }

    pub fn diff(&self, left: &str, right: &str) -> Result<RunDiff> {
        let store = self.store()?;
        Ok(compare::diff(
            &store.resolve_run(left)?,
            &store.resolve_run(right)?,
        ))
    }

    /// A receipt by full id or unambiguous hex prefix. One whose body no
    /// longer hashes to its id is refused: nothing it says can be believed.
    pub fn receipt(&self, spec: &str) -> Result<Receipt> {
        let receipt = self.store()?.find_receipt(spec)?;
        if !receipt.is_authentic() {
            return Err(crate::core::fault::integrity(format!(
                "receipt {} does not match its body; it was altered after it was recorded",
                receipt.receipt_id
            )));
        }
        Ok(receipt)
    }

    /// The digest of the complete output one step of a run printed: stdout,
    /// or stderr when `stderr`. Read it with [`Workspace::object`] or
    /// [`Workspace::read_object`]. By default that is the receipt the run
    /// kept; `which` can name another candidate the run executed, an earlier
    /// attempt, or both.
    pub fn step_output(
        &self,
        run: &str,
        step: &str,
        which: &OutputOf,
        stderr: bool,
    ) -> Result<String> {
        let manifest = self.store()?.resolve_run(run)?;
        let Some(entry) = manifest.steps.iter().find(|entry| entry.step_id == step) else {
            let steps: Vec<&str> = manifest
                .steps
                .iter()
                .map(|entry| entry.step_id.as_str())
                .collect();
            bail!(
                "run {} has no step `{step}`; its steps are {}",
                manifest.run_id,
                steps.join(", ")
            );
        };
        // The receipts one candidate's attempts left, oldest first.
        let mut tries: Vec<&str> = Vec::new();
        let kept = self.receipt(&entry.receipt_id)?;
        let other = match which.candidate.as_deref() {
            Some(name) if kept.identity.candidate.as_deref() != Some(name) => {
                let Some(other) = entry.candidates.iter().find(|c| c.name == name) else {
                    let ran: Vec<&str> = kept
                        .identity
                        .candidate
                        .as_deref()
                        .into_iter()
                        .chain(entry.candidates.iter().map(|c| c.name.as_str()))
                        .collect();
                    if ran.is_empty() {
                        bail!("step `{step}` of run {} has no candidates", manifest.run_id);
                    }
                    bail!(
                        "step `{step}` of run {} ran no candidate `{name}`; it ran {}",
                        manifest.run_id,
                        ran.join(", ")
                    );
                };
                Some(other)
            }
            _ => None,
        };
        match other {
            Some(other) => {
                tries.extend(other.attempts.iter().map(|a| a.receipt_id.as_str()));
                tries.push(&other.receipt_id);
            }
            None => {
                tries.extend(entry.attempts.iter().map(|a| a.receipt_id.as_str()));
                tries.push(&entry.receipt_id);
            }
        }
        let receipt_id = match which.attempt {
            None => tries[tries.len() - 1],
            Some(number) if (1..=tries.len()).contains(&number) => tries[number - 1],
            Some(number) => bail!(
                "attempt {number} is not recorded: step `{step}`{} of run {} took {} attempt(s)",
                which
                    .candidate
                    .as_deref()
                    .map(|name| format!(" candidate {name}"))
                    .unwrap_or_default(),
                manifest.run_id,
                tries.len()
            ),
        };
        let identity = if receipt_id == kept.receipt_id {
            kept.identity
        } else {
            self.receipt(receipt_id)?.identity
        };
        Ok(if stderr {
            identity.stderr_digest
        } else {
            identity.stdout_digest
        })
    }

    /// The complete bytes of a stored object, authenticated against its digest.
    pub fn object(&self, digest: &str) -> Result<Vec<u8>> {
        self.store()?.read_bytes(digest)
    }

    pub fn read_object(&self, digest: &str, max_bytes: usize) -> Result<ObjectView> {
        let bytes = self.object(digest)?;
        let (text, truncated) = exec::inline_preview(&bytes, max_bytes);
        Ok(ObjectView {
            digest: digest.to_string(),
            bytes: bytes.len(),
            truncated,
            text,
            recovery: truncated
                .then(|| format!("raise max_bytes above {} to read more", bytes.len())),
        })
    }

    /// Recorded runs, newest first.
    pub fn runs(&self, limit: usize) -> Result<RunList> {
        let store = self.store()?;
        let mut list = RunList::default();
        for run_id in store.list_runs()?.into_iter().take(limit) {
            // One unreadable run does not hide the others; it is named.
            match store.resolve_run(&run_id) {
                Ok(manifest) => list.runs.push(RunSummary {
                    imported: store.is_imported(&run_id),
                    run_id: manifest.run_id,
                    verdict: manifest.verdict,
                    root_digest: manifest.root_digest,
                    claims: manifest.claims,
                    steps: manifest.steps.len(),
                }),
                Err(error) => list.unreadable.push(format!("{run_id}: {error:#}")),
            }
        }
        Ok(list)
    }

    /// Search the newest `runs` recorded runs for `query`, literally and
    /// ignoring case; at most `limit` hits.
    pub fn search(&self, query: &str, runs: usize, limit: usize) -> Result<search::SearchReport> {
        search::search(&self.store()?, query, runs, limit)
    }

    /// Steps and proofs that gave different results on the same inputs in
    /// the newest `runs` runs executed here.
    pub fn flaky(&self, runs: usize) -> Result<flaky::FlakyReport> {
        flaky::flaky(&self.store()?, runs)
    }

    pub fn gc(&self, dry_run: bool) -> Result<GcReport> {
        self.store()?.gc(dry_run)
    }

    /// Attest a run executed here with the host signing key and record the
    /// attestation in the store, so `verify` reports it and every later
    /// bundle of the run carries it.
    pub fn sign(&self, run: &str) -> Result<SignReport> {
        let store = self.store()?;
        let manifest = store.resolve_run(run)?;
        sign_recorded(&store, &manifest, &keys::load(&keys::default_path()?)?)
    }

    /// Write one run as a portable bundle. Defaults to
    /// `<root>/<run-id>.flotbundle.json`. `sign` attests the run first, as
    /// `sign` does.
    pub fn bundle(&self, run: &str, out: Option<&Path>, share: Sharing) -> Result<ExportReport> {
        let store = self.store()?;
        let manifest = store.resolve_run(run)?;
        let path = out.map(Path::to_path_buf).unwrap_or_else(|| {
            self.root
                .join(format!("{}.flotbundle.json", manifest.run_id))
        });
        bundle::write(&shareable(&store, &manifest, share)?, &path)
    }

    /// Put a run on a git remote under [`share::RUNS_REF`]. `sign` attests
    /// the run first, as `sign` does.
    pub fn push(&self, run: &str, remote: &str, share: Sharing) -> Result<share::PushReport> {
        let store = self.store()?;
        let manifest = store.resolve_run(run)?;
        let bundle = shareable(&store, &manifest, share)?;
        share::push(&self.root, remote, &manifest, &bundle)
    }

    /// Import the runs of this flow that a git remote holds and this store
    /// does not.
    pub fn pull(&self, remote: &str, policy: ImportPolicy) -> Result<share::PullReport> {
        let flow = self.flow()?;
        share::pull(&self.root, &self.store()?, remote, &flow.meta.name, policy)
    }

    pub fn import(&self, path: &Path, policy: ImportPolicy) -> Result<ImportReport> {
        let store = self.store()?;
        bundle::import(&store, &bundle::read(path)?, policy)
    }

    /// Create the host signing key, or report the existing one.
    pub fn key_init() -> Result<KeyInfo> {
        let path = keys::default_path()?;
        let (key, created) = if path.exists() {
            (keys::load(&path)?, false)
        } else {
            (keys::generate(&path)?, true)
        };
        Ok(KeyInfo {
            public_key: key.public_key(),
            path,
            created,
        })
    }

    pub fn key_show() -> Result<KeyInfo> {
        let key = keys::load(&keys::default_path()?)?;
        Ok(KeyInfo {
            public_key: key.public_key(),
            path: key.path().to_path_buf(),
            created: false,
        })
    }

    /// Replace the host key with a new one, and write the handover both keys
    /// sign to `out` (default: `flot-handover-<new key>.json` here). The
    /// handover is written first: if it cannot be, the key is not replaced.
    pub fn key_rotate(out: Option<&Path>) -> Result<KeyRotation> {
        let path = keys::default_path()?;
        let rotation = keys::begin_rotation(&path)?;
        let handover = rotation.handover().clone();
        let out = match out {
            Some(out) => out.to_path_buf(),
            None => std::env::current_dir()?.join(format!(
                "flot-handover-{}.json",
                &handover.to[attest::KEY_PREFIX.len()..][..16]
            )),
        };
        let mut text = serde_json::to_string_pretty(&handover)?;
        text.push('\n');
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out)
            .and_then(|mut file| std::io::Write::write_all(&mut file, text.as_bytes()))
            .with_context(|| format!("cannot write the handover to {}", out.display()))?;
        let retired = rotation.retired_path();
        if let Err(error) = rotation.commit() {
            let _ = std::fs::remove_file(&out);
            return Err(error);
        }
        Ok(KeyRotation {
            path,
            old_key: handover.from,
            new_key: handover.to,
            retired,
            handover: out,
        })
    }

    /// Follow a trusted signer to the key their handover names. The new key
    /// is trusted under the old key's name; the old key stays trusted unless
    /// `revoke`, so the runs it already signed keep their authorship.
    pub fn trust_rotate(&self, path: &Path, revoke: bool) -> Result<TrustRotation> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let handover: attest::Handover = serde_json::from_str(&text)
            .with_context(|| format!("{} is not a flot key handover", path.display()))?;
        let (from, to) = attest::check_handover(&handover).map_err(|error| match error {
            attest::AttestError::BadSignature | attest::AttestError::MalformedSignature => {
                fault::integrity(format!("the handover does not authenticate: {error}"))
            }
            other => anyhow::anyhow!("{} is not a usable handover: {other}", path.display()),
        })?;
        let store = self.store()?;
        let mut trusted = store.trusted_keys()?;
        let name = match (trusted.get(&from), trusted.get(&to)) {
            (Some(old), Some(new)) if old != new => bail!(
                "{} is already trusted as {new}, not as {old}; remove one of them first",
                attest::fingerprint(&to)
            ),
            (Some(name), _) | (None, Some(name)) => name.clone(),
            (None, None) => bail!(
                "the handover is from {}, which this store does not trust, so it vouches for nothing here",
                attest::fingerprint(&from)
            ),
        };
        let added = trusted.insert(to.clone(), name.clone()).is_none();
        if revoke {
            trusted.remove(&from);
        }
        store.save_trusted_keys(&trusted)?;
        Ok(TrustRotation {
            name,
            revoked: !trusted.contains_key(&from),
            from,
            to,
            added,
        })
    }

    /// Believe a signer in this store. Re-adding a name replaces every key it
    /// stood for.
    pub fn trust_add(&self, name: &str, key: &str) -> Result<TrustEntry> {
        let name = name.trim();
        if name.is_empty() {
            bail!("a trusted signer needs a name");
        }
        let key = attest::canonical_key(key).context("cannot trust that key")?;
        let store = self.store()?;
        let mut trusted = store.trusted_keys()?;
        trusted.retain(|_, existing| existing != name);
        trusted.insert(key.clone(), name.to_string());
        store.save_trusted_keys(&trusted)?;
        Ok(TrustEntry {
            name: name.to_string(),
            key,
        })
    }

    /// Stop believing a signer, by name or key.
    pub fn trust_remove(&self, name_or_key: &str) -> Result<TrustEntry> {
        let store = self.store()?;
        let mut trusted = store.trusted_keys()?;
        let wanted = attest::canonical_key(name_or_key).ok();
        let matches: Vec<TrustEntry> = trusted
            .iter()
            .filter(|(key, name)| *name == name_or_key || Some(*key) == wanted.as_ref())
            .map(|(key, name)| TrustEntry {
                name: name.clone(),
                key: key.clone(),
            })
            .collect();
        // After a rotation one name can stand for two keys; say which.
        if matches.len() > 1 {
            bail!(
                "`{name_or_key}` names {} trusted keys; remove one by its key",
                matches.len()
            );
        }
        let found = matches
            .into_iter()
            .next()
            .with_context(|| format!("no trusted signer matches `{name_or_key}`"))?;
        trusted.remove(&found.key);
        store.save_trusted_keys(&trusted)?;
        Ok(found)
    }

    pub fn trust_list(&self) -> Result<Vec<TrustEntry>> {
        let mut entries: Vec<TrustEntry> = self
            .store()?
            .trusted_keys()?
            .into_iter()
            .map(|(key, name)| TrustEntry { name, key })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }
}

/// Walk up from `start` looking for a flow file; fall back to `start`.
fn find_flow_root(start: &Path, flow_file: &Path) -> PathBuf {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join(flow_file).is_file() {
            return dir.to_path_buf();
        }
        current = dir.parent();
    }
    start.to_path_buf()
}

/// A signature says "this host ran this, and this is what it recorded". It
/// cannot say that of a run that arrived in a bundle, nor of a run whose
/// evidence no longer authenticates: signing either would vouch for something
/// this host did not see.
fn sign_recorded(
    store: &Store,
    manifest: &RunManifest,
    key: &keys::SigningKeyFile,
) -> Result<SignReport> {
    signable(store, manifest)?;
    attest_recorded(store, manifest, key)
}

/// Whether this host may sign the run; see [`sign_recorded`].
fn signable(store: &Store, manifest: &RunManifest) -> Result<()> {
    if store.is_imported(&manifest.run_id) {
        bail!(
            "run {} was imported, not run here; it can be bundled again but not signed by this host",
            manifest.run_id
        );
    }
    let audit = audit::audit(store, manifest)?;
    if !audit.intact {
        return Err(fault::integrity(format!(
            "run {} does not authenticate, so this host will not sign it: {}",
            manifest.run_id,
            audit.findings.join("; ")
        )));
    }
    Ok(())
}

/// Record the host's signature over a run [`signable`] accepted.
fn attest_recorded(
    store: &Store,
    manifest: &RunManifest,
    key: &keys::SigningKeyFile,
) -> Result<SignReport> {
    let signer = key.public_key();
    let added = !store
        .attestations(&manifest.run_id)?
        .iter()
        .any(|existing| existing.key == signer);
    store.add_attestations(&manifest.run_id, &[key.sign(manifest)])?;
    Ok(SignReport {
        run_id: manifest.run_id.clone(),
        root_digest: manifest.root_digest.clone(),
        signer,
        added,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn find_flow_root_walks_upward() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        fs::write(dir.path().join("flot.toml"), "").unwrap();
        assert_eq!(
            find_flow_root(&nested, Path::new("flot.toml")),
            dir.path().to_path_buf()
        );
    }

    #[test]
    fn find_flow_root_falls_back_to_the_start_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            find_flow_root(dir.path(), Path::new("flot.toml")),
            dir.path().to_path_buf()
        );
    }

    #[test]
    fn a_named_flow_file_has_its_store_beside_it_however_it_is_named() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        let flow = dir.path().join("sub/custom.toml");
        fs::write(&flow, "").unwrap();
        for named in [flow.clone(), PathBuf::from("sub/custom.toml")] {
            let workspace = Workspace::locate(dir.path(), Some(&named)).unwrap();
            assert_eq!(workspace.root(), dir.path().join("sub"));
            assert_eq!(workspace.flow_path(), flow.as_path());
        }
        let missing = Workspace::locate(dir.path(), Some(Path::new("absent.toml")));
        assert!(missing.unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn init_never_writes_above_the_directory_it_runs_in() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("flot.toml"), "# parent").unwrap();
        let below = dir.path().join("sub");
        fs::create_dir_all(&below).unwrap();
        let workspace = Workspace::for_init(&below, None);
        assert_eq!(workspace.flow_path(), below.join("flot.toml"));
        workspace.init(true).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("flot.toml")).unwrap(),
            "# parent"
        );
    }

    #[test]
    fn init_refuses_to_overwrite_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::at(dir.path(), &dir.path().join("flot.toml"));
        workspace.init(false).unwrap();
        assert!(workspace.init(false).is_err());
        workspace.init(true).unwrap();
        assert!(workspace.flow().is_ok(), "the starter flow must parse");
    }
}
