//! The contract. Both adapters expose exactly these operations, so a person at
//! a terminal and an agent over MCP reach the same receipts, verdicts and
//! digests for the same request. Adapters only parse and render.

pub mod audit;
pub mod bundle;
pub mod doctor;
pub mod flaky;
pub mod keys;
pub mod search;
pub mod share;

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

/// A file whose change would touch the flow's proofs, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Guarded {
    /// As the guard names it: relative to the nearest flow's directory or
    /// its repository, from `~/` in the user's home directory, else absolute.
    pub path: String,
    pub reason: String,
}

/// Whether this platform's file systems usually ignore case, so that
/// `FLOT.TOML` is the flow file.
const FOLDS_CASE: bool = cfg!(any(windows, target_os = "macos"));

/// How many entries the guard reads below a directory a call changes, for
/// what no flow names in advance.
const GUARD_WALK_LIMIT: usize = 5000;

/// `text` as names are compared on this platform.
fn folded(text: &str) -> String {
    if FOLDS_CASE {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

/// `path` below `base`, or `base` itself as ``, `/`-separated. With `fold`,
/// a component matches whatever its case.
fn strip_base(path: &Path, base: &Path, fold: bool) -> Option<String> {
    let mut parts = path.components();
    for expected in base.components() {
        let part = parts.next()?;
        let same = part == expected
            || (fold
                && part.as_os_str().to_string_lossy().to_lowercase()
                    == expected.as_os_str().to_string_lossy().to_lowercase());
        if !same {
            return None;
        }
    }
    Some(
        parts
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// `path` below the first of `bases` that holds it, `/`-separated.
fn below(path: &Path, bases: &[PathBuf]) -> Option<String> {
    bases
        .iter()
        .find_map(|base| strip_base(path, base, FOLDS_CASE))
}

/// `dir` as named, and as resolved when that differs.
fn spellings(dir: &Path) -> Vec<PathBuf> {
    let named = normalize(dir);
    let resolved = resolve(&named);
    if resolved == named {
        vec![named]
    } else {
        vec![named, resolved]
    }
}

/// Where `path` leads: through every link, for as much of it as exists, and
/// as named below that. A link to a file that does not exist yet leads to
/// where writing through it would create one.
fn resolve(path: &Path) -> PathBuf {
    resolve_through(path, 8)
}

fn resolve_through(path: &Path, links: u8) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    if links > 0 {
        if let Ok(target) = std::fs::read_link(path) {
            let parent = path.parent().unwrap_or(Path::new(""));
            return resolve_through(&normalize(&parent.join(target)), links - 1);
        }
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => resolve_through(parent, links).join(name),
        _ => path.to_path_buf(),
    }
}

/// Why `relative` (to a flow's repository, or its directory outside one)
/// decides whether the evidence is checked at all: the agent's settings,
/// which run the hook that enforces the guard, wherever in the repository
/// Claude Code is started; CI workflows, which run the flow; and git hooks
/// and configuration, which run code on their own.
fn harness(relative: &str) -> Option<String> {
    let relative = folded(relative);
    let parts: Vec<&str> = relative.split('/').collect();
    let reason = match parts.as_slice() {
        [.., ".claude", name] if is_settings(name) => {
            "it configures Claude Code, including the hook that enforces this guard"
        }
        [".git", "config"] => "it is git configuration, which can point git at other hooks",
        [".git", "hooks"] => "it holds git hooks, code git runs on its own",
        [".git", "hooks", ..] => "it is a git hook, code git runs on its own",
        [".github", "workflows"] => "it holds CI workflows, which decide what CI checks",
        [".github", "workflows", ..] => "it is a CI workflow, which decides what CI checks",
        _ => return None,
    };
    Some(reason.to_string())
}

/// Why `relative` (to the user's home directory) decides whether the guard
/// runs at all.
fn user_harness(relative: &str) -> Option<String> {
    let relative = folded(relative);
    let parts: Vec<&str> = relative.split('/').collect();
    let reason = match parts.as_slice() {
        [".claude", name] if is_settings(name) => {
            "it configures Claude Code for this user, including the hook that enforces this guard"
        }
        [".claude", "plugins"] => {
            "it holds this user's Claude Code plugins, one of which installs the hook that enforces this guard"
        }
        [".claude", "plugins", ..] => {
            "it is a Claude Code plugin for this user, or part of one, which can install or remove the hook that enforces this guard"
        }
        _ => return None,
    };
    Some(reason.to_string())
}

/// `settings.json`, `settings.local.json` and the like, which Claude Code
/// reads.
fn is_settings(name: &str) -> bool {
    name.starts_with("settings") && name.ends_with(".json")
}

/// How the guard matches a proof pattern: as a flow does, ignoring case
/// where the file system does.
fn proof_match() -> glob::MatchOptions {
    glob::MatchOptions {
        case_sensitive: !FOLDS_CASE,
        ..crate::engine::keying::match_options()
    }
}

/// What `pattern` matches below `dir` on disk; a pattern without wildcards
/// is the one path it names.
fn on_disk(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    if !pattern.contains(['*', '?', '[']) {
        return vec![dir.join(pattern)];
    }
    let pattern = format!(
        "{}/{pattern}",
        glob::Pattern::escape(&dir.to_string_lossy())
    );
    glob::glob_with(&pattern, proof_match())
        .map(|paths| paths.flatten().take(GUARD_WALK_LIMIT).collect())
        .unwrap_or_default()
}

/// The files below `dir`, without following links, up to `limit` entries.
#[cfg(unix)]
fn files_below(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut queue = std::collections::VecDeque::from([dir.to_path_buf()]);
    let mut read = 0;
    while let Some(next) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            read += 1;
            if read > limit {
                return files;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => queue.push_back(entry.path()),
                Ok(kind) if kind.is_file() => files.push(entry.path()),
                _ => {}
            }
        }
    }
    files
}

/// One flow as the guard reads it, once per call.
struct Guarding {
    workspace: Workspace,
    /// The flow file, resolved, which tells two spellings of one flow apart.
    key: PathBuf,
    /// Each proof pattern, compiled and as written, with its step.
    proofs: Vec<(glob::Pattern, String, String)>,
    /// The flow's directory, as named and as resolved.
    roots: Vec<PathBuf>,
    /// The repository the flow is in, which may hold it in a subdirectory,
    /// else the flow's own directory; as named and as resolved.
    tops: Vec<PathBuf>,
}

impl Guarding {
    /// Why `relative` (to the flow's directory) is part of this flow's
    /// evidence, if it is.
    fn evidence(&self, relative: &str) -> Option<String> {
        let name = folded(relative);
        let flow_file = self
            .workspace
            .flow_path
            .file_name()
            .map(|file| folded(&file.to_string_lossy()));
        let store = crate::store::STORE_DIR;
        let reason = if Some(&name) == flow_file.as_ref() {
            "it is the flow file, which says what proves the work".to_string()
        } else if name == store || name.starts_with(&format!("{store}/")) {
            "it is in the flot store, which holds the evidence".to_string()
        } else if let Some((_, _, step)) = self
            .proofs
            .iter()
            .find(|(pattern, _, _)| pattern.matches_with(relative, proof_match()))
        {
            format!("the proof of step `{step}` reads it")
        } else if name.rsplit('/').next() == Some(DEFAULT_FLOW_FILE) {
            "it would be another flow file in this flow's project, which can change what proves the work and which flow guards it".to_string()
        } else {
            return None;
        };
        Some(reason)
    }
}

/// What one call is judged against: every flow that could own a file it
/// changes, each read once, and what outside them decides whether proofs
/// are checked: the user's Claude Code settings and plugins, and flot.
pub struct Guard {
    /// Nearest first: a file is named, and guarded, by the deepest flow
    /// that holds it.
    flows: Vec<Guarding>,
    /// Directories already looked in for a flow file.
    looked: std::collections::HashSet<PathBuf>,
    home: Vec<PathBuf>,
    /// The flot binary running the guard, resolved.
    exe: Option<PathBuf>,
}

impl Guard {
    /// A guard of `workspace`'s flow, if it has one; [`Guard::consult`] adds
    /// the others.
    pub fn new(workspace: &Workspace) -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        let mut guard = Guard {
            flows: Vec::new(),
            looked: std::collections::HashSet::new(),
            home: home.map(|home| spellings(&home)).unwrap_or_default(),
            exe: std::env::current_exe().ok().map(|exe| resolve(&exe)),
        };
        guard.add(workspace);
        guard
    }

    fn add(&mut self, workspace: &Workspace) {
        if !workspace.flow_path.is_file() {
            return;
        }
        let key = resolve(&workspace.flow_path);
        let top = workspace
            .root
            .ancestors()
            .find(|dir| dir.join(".git").exists())
            .unwrap_or(&workspace.root);
        // A flow reached through a link too names its files from there.
        if let Some(known) = self.flows.iter_mut().find(|known| known.key == key) {
            for (spellings, dir) in [
                (&mut known.roots, workspace.root.as_path()),
                (&mut known.tops, top),
            ] {
                for spelling in self::spellings(dir) {
                    if !spellings.contains(&spelling) {
                        spellings.push(spelling);
                    }
                }
            }
            return;
        }
        // A flow file that does not parse still guards itself, the store
        // and the harness.
        let proofs = workspace
            .flow()
            .map(|flow| {
                flow.steps
                    .iter()
                    .flat_map(|step| {
                        step.verify_inputs.iter().filter_map(|pattern| {
                            let compiled = glob::Pattern::new(pattern).ok()?;
                            Some((compiled, pattern.clone(), step.id.clone()))
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.flows.push(Guarding {
            workspace: workspace.clone(),
            key,
            proofs,
            roots: spellings(&workspace.root),
            tops: spellings(top),
        });
        self.flows
            .sort_by_key(|flow| std::cmp::Reverse(flow.roots[0].components().count()));
    }

    /// Add every flow file from `dir` up to the top of its repository: one
    /// that can be created below a flow does not hide the flow above it.
    pub fn consult(&mut self, dir: &Path) {
        for spelling in spellings(dir) {
            for ancestor in spelling.ancestors() {
                if !self.looked.insert(ancestor.to_path_buf()) {
                    break;
                }
                let flow_path = ancestor.join(DEFAULT_FLOW_FILE);
                if flow_path.is_file() {
                    self.add(&Workspace::beside(&flow_path));
                }
                if ancestor.join(".git").exists() {
                    break;
                }
            }
        }
    }

    /// Whether any flow guards the call. Where none does, nothing is flot's
    /// to refuse.
    pub fn active(&self) -> bool {
        !self.flows.is_empty()
    }

    /// The flows found, for a refusal to be noted in.
    pub fn workspaces(&self) -> impl Iterator<Item = &Workspace> {
        self.flows.iter().map(|flow| &flow.workspace)
    }

    /// Why changing `path` (absolute), or anything below it, would touch a
    /// flow's proofs or decide whether they are checked, if it would. The
    /// path is judged as named, then where it leads through links, then as
    /// another name of a guarded file, then, for a directory, by what it
    /// holds.
    pub fn judge(&self, path: &Path) -> Option<Guarded> {
        if self.flows.is_empty() {
            return None;
        }
        let named = normalize(path);
        if let Some((path, reason)) = self.rule(&named) {
            return Some(Guarded { path, reason });
        }
        let resolved = resolve(&named);
        let reason = (resolved != named)
            .then(|| self.rule(&resolved))
            .flatten()
            .map(|(to, reason)| format!("it leads to {to}: {reason}"))
            .or_else(|| {
                self.hard_link(&resolved)
                    .map(|(to, reason)| format!("it is a hard link to {to}: {reason}"))
            })
            .or_else(|| resolved.is_dir().then(|| self.holds(&resolved)).flatten())?;
        Some(Guarded {
            path: self.name(&named),
            reason,
        })
    }

    /// The rule that guards `path` as it is named, without following links
    /// or looking below it: the name it goes by, from the flow or
    /// repository whose rule it is, and why.
    fn rule(&self, path: &Path) -> Option<(String, String)> {
        for flow in &self.flows {
            if let Some(relative) = below(path, &flow.roots) {
                if let Some(reason) = flow.evidence(&relative) {
                    return Some((relative, reason));
                }
            }
        }
        for flow in &self.flows {
            if let Some(relative) = below(path, &flow.tops) {
                if let Some(reason) = harness(&relative) {
                    return Some((relative, reason));
                }
            }
        }
        if let Some(relative) = below(path, &self.home) {
            if let Some(reason) = user_harness(&relative) {
                return Some((format!("~/{relative}"), reason));
            }
        }
        let exe = self.exe.as_deref()?;
        (strip_base(path, exe, FOLDS_CASE).as_deref() == Some("")).then(|| {
            (
                path.display().to_string(),
                "it is the flot binary that enforces this guard".to_string(),
            )
        })
    }

    /// `path` as the guard names it.
    fn name(&self, path: &Path) -> String {
        let relative = self
            .flows
            .iter()
            .find_map(|flow| below(path, &flow.roots))
            .or_else(|| self.flows.iter().find_map(|flow| below(path, &flow.tops)));
        match relative {
            Some(relative) if relative.is_empty() => ".".to_string(),
            Some(relative) => relative,
            None => match below(path, &self.home) {
                Some(relative) if relative.is_empty() => "~".to_string(),
                Some(relative) => format!("~/{relative}"),
                None => path.display().to_string(),
            },
        }
    }

    /// Where guarded files are known to be, as far as they exist: each
    /// flow's file, store and proofs (a pattern by its matches), its
    /// repository's harness, the user's Claude Code settings and plugins,
    /// and flot itself.
    fn places(&self) -> Vec<PathBuf> {
        let mut places = Vec::new();
        for flow in &self.flows {
            let root = &flow.workspace.root;
            places.push(flow.workspace.flow_path.clone());
            places.push(root.join(crate::store::STORE_DIR));
            for (_, pattern, _) in &flow.proofs {
                places.extend(on_disk(root, pattern));
            }
            let top = &flow.tops[0];
            for place in [".git/config", ".git/hooks", ".github/workflows"] {
                places.push(top.join(place));
            }
            for dir in [top, root] {
                places.extend(on_disk(dir, ".claude/settings*.json"));
            }
        }
        if let Some(home) = self.home.first() {
            places.extend(on_disk(home, ".claude/settings*.json"));
            places.push(home.join(".claude/plugins"));
        }
        places.extend(self.exe.clone());
        let mut seen = std::collections::HashSet::new();
        places.retain(|place| place.symlink_metadata().is_ok() && seen.insert(place.clone()));
        places
    }

    /// What the directory `dir` (resolved) holds that is guarded, as a
    /// reason, if anything: what is known to be there, then what a bounded
    /// walk finds that no flow names in advance, such as another flow's
    /// file or Claude Code settings deeper down.
    fn holds(&self, dir: &Path) -> Option<String> {
        let mut held: Vec<(String, String)> = Vec::new();
        let mut note = |found: (String, String)| {
            if !held.iter().any(|(name, _)| *name == found.0) {
                held.push(found);
            }
        };
        for place in self.places() {
            let resolved = resolve(&place);
            if strip_base(&resolved, dir, FOLDS_CASE).is_some() {
                if let Some(found) = self.rule(&place).or_else(|| self.rule(&resolved)) {
                    note(found);
                }
            }
        }
        let mut queue = std::collections::VecDeque::from([dir.to_path_buf()]);
        let mut read = 0;
        'walk: while let Some(next) = queue.pop_front() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries.flatten() {
                read += 1;
                if read > GUARD_WALK_LIMIT {
                    break 'walk;
                }
                let path = entry.path();
                if let Some(found) = self.rule(&path) {
                    note(found);
                } else if entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && entry.file_name() != ".git"
                {
                    queue.push_back(path);
                }
            }
        }
        let ((name, reason), rest) = held.split_first()?;
        let mut said = format!("it holds {name}: {reason}");
        if !rest.is_empty() {
            let shown: Vec<&str> = rest.iter().take(6).map(|(name, _)| name.as_str()).collect();
            said.push_str(&format!("; also {}", shown.join(", ")));
            if rest.len() > shown.len() {
                said.push_str(&format!(" and {} more", rest.len() - shown.len()));
            }
        }
        Some(said)
    }

    /// The guarded file that `path` (resolved) is another name of, by
    /// device and inode.
    #[cfg(unix)]
    fn hard_link(&self, path: &Path) -> Option<(String, String)> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() || meta.nlink() < 2 {
            return None;
        }
        let mut files = Vec::new();
        for place in self.places() {
            if place.is_dir() {
                files.extend(files_below(&place, GUARD_WALK_LIMIT));
            } else {
                files.push(place);
            }
        }
        files.into_iter().find_map(|file| {
            let other = std::fs::metadata(&file).ok()?;
            if (other.dev(), other.ino()) != (meta.dev(), meta.ino()) {
                return None;
            }
            self.rule(&file).or_else(|| self.rule(&resolve(&file)))
        })
    }

    #[cfg(not(unix))]
    fn hard_link(&self, _path: &Path) -> Option<(String, String)> {
        None
    }
}

/// What git is asked, to learn which files a command would change.
enum GitAsk {
    /// The changes in the working tree and index, and untracked files too.
    Status { untracked: bool },
    /// The files that may change, for each way they may: those that differ
    /// in every one of its comparisons, each of two revisions or of a
    /// revision (or, with none, the index) and the working tree. A three-way
    /// merge, for one, changes only what both sides changed.
    Diff(Vec<Vec<Vec<String>>>),
    /// The files these patches touch, with the options that place them.
    Apply(Vec<String>),
    /// What `git clean` with these options would remove.
    Clean(Vec<String>),
    /// Nothing git can answer, and why.
    Unknown(&'static str),
}

/// The operands and options of a git subcommand's `args`, past the options
/// in `valued` that take the next word as their value, and the paths after
/// `--`, if any.
fn git_operands<'a>(
    args: &'a [String],
    valued: &[&str],
) -> (Vec<&'a str>, Vec<&'a str>, Option<Vec<&'a str>>) {
    let mut operands = Vec::new();
    let mut options = Vec::new();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        at += 1;
        if arg == "--" {
            let paths = args[at..].iter().map(String::as_str).collect();
            return (operands, options, Some(paths));
        }
        if arg.starts_with('-') && arg.len() > 1 {
            options.push(arg.as_str());
            if valued.contains(&arg.as_str()) {
                at += 1;
            }
        } else {
            operands.push(arg.as_str());
        }
    }
    (operands, options, None)
}

fn stash_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, _) = git_operands(args, &["-m", "--message"]);
    let untracked = options
        .iter()
        .any(|option| matches!(*option, "-u" | "--include-untracked" | "-a" | "--all"));
    match operands.first().copied() {
        None | Some("push" | "save") => Some(GitAsk::Status { untracked }),
        Some("pop" | "apply") => {
            let entry = operands.get(1).copied().unwrap_or("stash@{0}");
            Some(GitAsk::Diff(vec![vec![
                revisions(&[&format!("{entry}^1"), entry]),
                revisions(&["HEAD", entry]),
            ]]))
        }
        Some("branch") => Some(GitAsk::Unknown(
            "`git stash branch` checks out another revision",
        )),
        // list, show, drop, clear, create, store: the working tree stays.
        _ => None,
    }
}

fn reset_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, _) = git_operands(args, &[]);
    if !options
        .iter()
        .any(|option| matches!(*option, "--hard" | "--keep" | "--merge"))
    {
        return None;
    }
    let revision = operands.first().copied().unwrap_or("HEAD");
    Some(GitAsk::Diff(vec![vec![revisions(&[revision])]]))
}

/// One comparison `git diff` makes.
fn revisions(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// `checkout` or `switch` to a revision. With paths, the command line names
/// what it writes.
fn switch_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, paths) =
        git_operands(args, &["-b", "-B", "-c", "-C", "--orphan", "--conflict"]);
    if options
        .iter()
        .any(|option| option.starts_with("--pathspec-from-file"))
    {
        return Some(GitAsk::Unknown("it reads its paths from a file"));
    }
    if paths.is_some() || operands.len() > 1 {
        return None;
    }
    let revision = operands.first().copied().map(|revision| match revision {
        "-" => "@{-1}",
        revision => revision,
    });
    // `-p` takes back what the working tree changed from the index, or
    // from the revision.
    if options
        .iter()
        .any(|option| matches!(*option, "-p" | "--patch"))
    {
        return Some(GitAsk::Diff(vec![vec![revisions(
            &revision.into_iter().collect::<Vec<_>>(),
        )]]));
    }
    // Local changes stay across a switch unless it is forced.
    let force = options
        .iter()
        .any(|option| matches!(*option, "-f" | "--force" | "--discard-changes"));
    let revision = match revision {
        Some(revision) => revision,
        None if force => "HEAD",
        None => return None,
    };
    Some(GitAsk::Diff(vec![vec![if force {
        revisions(&[revision])
    } else {
        revisions(&["HEAD", revision])
    }]]))
}

/// Whether `options` go on with, or back out of, a merge, rebase,
/// cherry-pick, revert or `am` that already started; that changes no more
/// than starting it did, or takes it back.
fn resumes(options: &[&str]) -> bool {
    options
        .iter()
        .any(|option| matches!(*option, "--abort" | "--continue" | "--quit" | "--skip"))
}

/// `merge` of revisions, by default the upstream: what each brings since
/// it and `HEAD` parted.
fn merge_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, _) = git_operands(
        args,
        &[
            "-m",
            "-F",
            "--file",
            "-s",
            "--strategy",
            "-X",
            "--strategy-option",
            "--into-name",
            "--cleanup",
        ],
    );
    if resumes(&options) {
        return None;
    }
    let revisions = if operands.is_empty() {
        vec!["@{upstream}"]
    } else {
        operands
    };
    Some(GitAsk::Diff(
        revisions
            .into_iter()
            .map(|revision| {
                vec![
                    vec![format!("HEAD...{revision}")],
                    self::revisions(&["HEAD", revision]),
                ]
            })
            .collect(),
    ))
}

/// `rebase`: what differs between the branch and where it lands, and what
/// both the commits it moves and that place changed, which may conflict;
/// and, when it names a branch, what switching to it changes.
fn rebase_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, _) = git_operands(
        args,
        &[
            "--onto",
            "-s",
            "--strategy",
            "-X",
            "--strategy-option",
            "-x",
            "--exec",
        ],
    );
    if resumes(&options)
        || options.contains(&"--edit-todo")
        || options.contains(&"--show-current-patch")
    {
        return None;
    }
    if options.iter().any(|option| {
        matches!(*option, "-x" | "--root" | "-i" | "--interactive") || option.starts_with("--exec")
    }) {
        return Some(GitAsk::Unknown(
            "an interactive rebase, or one that runs commands, does what it is told as it goes",
        ));
    }
    let upstream = operands.first().copied().unwrap_or("@{upstream}");
    let branch = operands.get(1).copied().unwrap_or("HEAD");
    let onto = args
        .iter()
        .position(|arg| arg == "--onto")
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
        .or_else(|| args.iter().find_map(|arg| arg.strip_prefix("--onto=")))
        .unwrap_or(upstream);
    let mut changes = vec![
        vec![revisions(&[branch, onto])],
        vec![
            vec![format!("{upstream}...{branch}")],
            vec![format!("{branch}...{onto}")],
        ],
    ];
    if branch != "HEAD" {
        changes.push(vec![revisions(&["HEAD", branch])]);
    }
    Some(GitAsk::Diff(changes))
}

/// `cherry-pick` or, with `revert`, the reverse of commits, or of ranges
/// of them: what each changes that `HEAD` does not already hold.
fn pick_changes(args: &[String], revert: bool) -> Option<GitAsk> {
    let (operands, options, _) = git_operands(
        args,
        &[
            "-m",
            "--mainline",
            "-s",
            "--strategy",
            "-X",
            "--strategy-option",
            "--cleanup",
        ],
    );
    if resumes(&options) {
        return None;
    }
    if options.contains(&"--stdin") || operands.is_empty() {
        return Some(GitAsk::Unknown("it reads its commits from standard input"));
    }
    let parent = args
        .iter()
        .position(|arg| arg == "-m" || arg == "--mainline")
        .and_then(|at| args.get(at + 1))
        .map_or("1", String::as_str);
    Some(GitAsk::Diff(
        operands
            .into_iter()
            .map(|operand| match operand.split_once("..") {
                Some((from, to)) => {
                    let to = to.trim_start_matches('.');
                    vec![vec![format!(
                        "{}...{}",
                        if from.is_empty() { "HEAD" } else { from },
                        if to.is_empty() { "HEAD" } else { to }
                    )]]
                }
                None => {
                    let before = format!("{operand}^{parent}");
                    let after = if revert { before.as_str() } else { operand };
                    vec![revisions(&[&before, operand]), revisions(&["HEAD", after])]
                }
            })
            .collect(),
    ))
}

/// `am` of mailboxes: the files their patches touch.
fn am_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, _) = git_operands(args, &[]);
    if resumes(&options) || options.contains(&"--show-current-patch") {
        return None;
    }
    if operands.is_empty() {
        return Some(GitAsk::Unknown("it reads the patches from standard input"));
    }
    // The options `git apply` shares place the paths.
    let mut ask: Vec<String> = options
        .into_iter()
        .filter(|option| {
            (option.starts_with("-p") && option.len() > 2)
                || option.starts_with("--directory=")
                || option.starts_with("--exclude=")
                || option.starts_with("--include=")
        })
        .map(str::to_string)
        .collect();
    ask.extend(operands.into_iter().map(str::to_string));
    Some(GitAsk::Apply(ask))
}

/// `clean`, unless it only says what it would remove.
fn clean_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, paths) = git_operands(args, &["-e", "--exclude"]);
    if options
        .iter()
        .any(|option| matches!(*option, "-n" | "--dry-run"))
    {
        return None;
    }
    // The options that choose what is removed stay; those that force or
    // ask go, and `--dry-run` takes their place.
    let mut ask = vec!["--dry-run".to_string()];
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        at += 1;
        match arg.as_str() {
            "--" => break,
            "-e" | "--exclude" => {
                ask.push(arg.clone());
                ask.extend(args.get(at).cloned());
                at += 1;
            }
            "-d" | "-x" | "-X" => ask.push(arg.clone()),
            _ if arg.starts_with("--exclude=") || arg.starts_with("-e") && arg.len() > 2 => {
                ask.push(arg.clone())
            }
            // Bundled short options, such as `-fdx`.
            _ if arg.starts_with('-') && !arg.starts_with("--") => {
                for flag in arg[1..]
                    .chars()
                    .filter(|flag| matches!(flag, 'd' | 'x' | 'X'))
                {
                    ask.push(format!("-{flag}"));
                }
            }
            _ => {}
        }
    }
    ask.push("--".to_string());
    ask.extend(
        operands
            .into_iter()
            .chain(paths.unwrap_or_default())
            .map(str::to_string),
    );
    Some(GitAsk::Clean(ask))
}

fn apply_changes(args: &[String]) -> GitAsk {
    let (operands, options, paths) = git_operands(args, &[]);
    let patches: Vec<&str> = operands
        .into_iter()
        .chain(paths.unwrap_or_default())
        .collect();
    if patches.is_empty() || patches.contains(&"-") {
        return GitAsk::Unknown("it reads the patch from standard input");
    }
    // The options that place the paths stay; those that print or apply
    // instead go.
    let mut ask: Vec<String> = options
        .into_iter()
        .filter(|option| {
            !matches!(
                *option,
                "--stat"
                    | "--numstat"
                    | "--summary"
                    | "--check"
                    | "--apply"
                    | "-v"
                    | "--verbose"
                    | "--index"
                    | "--cached"
                    | "-3"
                    | "--3way"
                    | "--reject"
            )
        })
        .map(str::to_string)
        .collect();
    ask.extend(patches.into_iter().map(str::to_string));
    GitAsk::Apply(ask)
}

/// What a whole-tree git command would change: `git` run in `dir` with
/// `args`, the words after `git`. `stash`, `reset --hard`, `checkout` or
/// `switch` to a revision, `checkout -f` or `-p`, `apply`, `am`, `merge`,
/// `rebase`, `cherry-pick`, `revert` and `clean` change files the command
/// line does not name, so git is asked which. `None` for any other git
/// command, whose files the line names itself; an error when git cannot
/// say, as for `pull`, which fetches first: a change the guard cannot rule
/// out.
pub fn git_would_change(dir: &Path, args: &[String]) -> Option<Result<Vec<PathBuf>>> {
    // The options that choose the repository and its working tree are
    // passed on; `-c` and the like are not, so a command line cannot
    // configure the git the guard runs.
    let mut place: Vec<String> = Vec::new();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        match arg.as_str() {
            "-C" | "--git-dir" | "--work-tree" => {
                place.push(arg.clone());
                place.extend(args.get(at + 1).cloned());
                at += 2;
            }
            "-c" | "--namespace" | "--config-env" | "--super-prefix" => at += 2,
            _ if arg.starts_with("--git-dir=") || arg.starts_with("--work-tree=") => {
                place.push(arg.clone());
                at += 1;
            }
            _ if arg.starts_with('-') => at += 1,
            _ => break,
        }
    }
    let (command, rest) = args.get(at..)?.split_first()?;
    let ask = match command.as_str() {
        "stash" => stash_changes(rest)?,
        "reset" => reset_changes(rest)?,
        "checkout" | "switch" => switch_changes(rest)?,
        "apply" => apply_changes(rest),
        "merge" => merge_changes(rest)?,
        "rebase" => rebase_changes(rest)?,
        "cherry-pick" => pick_changes(rest, false)?,
        "revert" => pick_changes(rest, true)?,
        "am" => am_changes(rest)?,
        "clean" => clean_changes(rest)?,
        "pull" => GitAsk::Unknown(
            "`git pull` fetches first; run `git fetch`, then `git merge` or `git rebase`, which can be judged",
        ),
        _ => return None,
    };
    Some(ask_git(dir, &place, &ask))
}

fn ask_git(dir: &Path, place: &[String], ask: &GitAsk) -> Result<Vec<PathBuf>> {
    if let GitAsk::Unknown(why) = ask {
        bail!("{why}");
    }
    let git = |args: &[&str]| -> Result<Vec<u8>> {
        let output = std::process::Command::new("git")
            .args(["-c", "core.fsmonitor=false", "-C"])
            .arg(dir)
            .args(place)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(std::process::Stdio::null())
            .output()
            .context("cannot run git")?;
        if !output.status.success() {
            let said = String::from_utf8_lossy(&output.stderr);
            bail!(
                "{}",
                said.lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("git failed")
                    .trim()
            );
        }
        Ok(output.stdout)
    };
    // `git apply` also patches outside a repository, from where it runs.
    let top = match (git(&["rev-parse", "--show-toplevel"]), ask) {
        (Ok(top), _) => PathBuf::from(String::from_utf8_lossy(&top).trim()),
        (Err(_), GitAsk::Apply(_)) => dir.to_path_buf(),
        (Err(error), _) => return Err(error),
    };
    let names = match ask {
        GitAsk::Status { untracked } => status_paths(&git(&[
            "status",
            "--porcelain",
            "-z",
            if *untracked {
                "--untracked-files=all"
            } else {
                "--untracked-files=no"
            },
        ])?),
        GitAsk::Diff(changes) => {
            let mut names: Vec<String> = Vec::new();
            for comparisons in changes {
                let mut common: Option<Vec<String>> = None;
                for revisions in comparisons {
                    let mut args = vec!["diff", "--name-only", "--no-renames", "-z"];
                    args.extend(revisions.iter().map(String::as_str));
                    args.push("--");
                    let differ: Vec<String> = git(&args)?
                        .split(|byte| *byte == 0)
                        .filter(|name| !name.is_empty())
                        .map(|name| String::from_utf8_lossy(name).into_owned())
                        .collect();
                    common = Some(match common {
                        None => differ,
                        Some(known) => known
                            .into_iter()
                            .filter(|name| differ.contains(name))
                            .collect(),
                    });
                }
                names.extend(common.unwrap_or_default());
            }
            names
        }
        // Named from where git runs, not from the top.
        GitAsk::Clean(options) => {
            let mut args = vec!["-c", "core.quotePath=false", "clean"];
            args.extend(options.iter().map(String::as_str));
            let mut cwd = dir.to_path_buf();
            for pair in place.windows(2) {
                if pair[0] == "-C" {
                    cwd = cwd.join(&pair[1]);
                }
            }
            let mut removed = Vec::new();
            for line in String::from_utf8_lossy(&git(&args)?).lines() {
                let Some(name) = line.strip_prefix("Would remove ") else {
                    continue;
                };
                if name.starts_with('"') {
                    bail!("git quotes the name {name}");
                }
                removed.push(cwd.join(name));
            }
            return Ok(removed);
        }
        GitAsk::Apply(patches) => {
            let mut args = vec!["apply", "--numstat", "-z"];
            args.extend(patches.iter().map(String::as_str));
            numstat_paths(&git(&args)?)
        }
        GitAsk::Unknown(_) => Vec::new(),
    };
    Ok(names.into_iter().map(|name| top.join(name)).collect())
}

/// The paths in `git status --porcelain -z`: each entry's, and a rename's
/// source as well.
fn status_paths(out: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut records = out
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        paths.push(String::from_utf8_lossy(&record[3..]).into_owned());
        if matches!(record[0], b'R' | b'C') {
            paths.extend(
                records
                    .next()
                    .map(|from| String::from_utf8_lossy(from).into_owned()),
            );
        }
    }
    paths
}

/// The paths in `git apply --numstat -z`: `added TAB deleted TAB path`, or
/// for a rename an empty path followed by both names.
fn numstat_paths(out: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut fields = out.split(|byte| *byte == 0);
    while let Some(field) = fields.next() {
        if field.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(field);
        match text.splitn(3, '\t').nth(2) {
            Some(path) if !path.is_empty() => paths.push(path.to_string()),
            _ => {
                for _ in 0..2 {
                    paths.extend(
                        fields
                            .next()
                            .map(|name| String::from_utf8_lossy(name).into_owned()),
                    );
                }
            }
        }
    }
    paths
}

/// `path` without `.` and with `..` applied, without touching the disk: the
/// file may not exist yet.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
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
    fn a_path_is_named_below_a_base_in_any_case_where_names_fold() {
        let base = Path::new("/r/Proj");
        assert_eq!(
            strip_base(Path::new("/r/Proj/a/b"), base, false).as_deref(),
            Some("a/b")
        );
        assert_eq!(
            strip_base(Path::new("/r/Proj"), base, false).as_deref(),
            Some("")
        );
        assert_eq!(strip_base(Path::new("/r/proj/a"), base, false), None);
        assert_eq!(
            strip_base(Path::new("/r/proj/A"), base, true).as_deref(),
            Some("A")
        );
        assert_eq!(strip_base(Path::new("/r/Projects/a"), base, true), None);
    }

    #[test]
    fn the_files_git_names_are_read_from_its_output() {
        assert_eq!(
            status_paths(b" M check.sh\0R  new.sh\0old.sh\0?? x y\0"),
            ["check.sh", "new.sh", "old.sh", "x y"]
        );
        assert_eq!(
            numstat_paths(b"1\t1\ta\x002\t0\t\0from\0to\0"),
            ["a", "from", "to"]
        );
    }

    #[test]
    fn a_merge_or_a_pick_changes_only_what_head_does_not_already_hold() {
        let words = |line: &str| line.split(' ').map(str::to_string).collect::<Vec<_>>();
        let Some(GitAsk::Diff(changes)) = merge_changes(&words("-m msg topic")) else {
            panic!("a merge is asked of git");
        };
        assert_eq!(changes, [[vec!["HEAD...topic"], vec!["HEAD", "topic"]]]);
        let Some(GitAsk::Diff(changes)) = pick_changes(&words("abc"), true) else {
            panic!("a revert is asked of git");
        };
        assert_eq!(changes, [[vec!["abc^1", "abc"], vec!["HEAD", "abc^1"]]]);
        assert!(merge_changes(&words("--abort")).is_none());
        assert!(clean_changes(&words("-n -fdx")).is_none());
        let Some(GitAsk::Clean(asked)) = clean_changes(&words("-fdx -e keep")) else {
            panic!("a clean is asked of git");
        };
        assert_eq!(asked, ["--dry-run", "-d", "-x", "-e", "keep", "--"]);
    }

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
