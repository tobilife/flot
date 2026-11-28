//! The contract. Both adapters expose exactly these operations, so a person at
//! a terminal and an agent over MCP reach the same receipts, verdicts and
//! digests for the same request. Adapters only parse and render.

pub mod audit;
pub mod bundle;
pub mod doctor;
pub mod explain;
pub mod flaky;
mod guard;
mod init;
pub mod keys;
mod output;
mod runs;
pub mod search;
pub mod share;
mod sharing;
mod signers;

pub use guard::{answer, judge, tilde, Answer, Call, Guarded, Judged, Refused, Shell};
pub use output::{ObjectView, OutputOf, Stream};
pub use runs::{Plan, RunDetail, RunList, RunSummary};
pub use sharing::{Sharing, SignReport};
pub use signers::{KeyInfo, KeyRotation, TrustEntry, TrustRotation};

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::core::flow::Flow;
use crate::core::receipt::Refusal;
// The request and report types are part of this contract; adapters take them
// from here and never reach into the engine.
pub use crate::core::fault::FaultKind;
pub use crate::engine::exec::{install_interrupt_handlers, interrupted, StopSignal};
/// What a run reports while it happens, for an adapter to show.
pub use crate::engine::progress;

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
use crate::store::Store;

pub const DEFAULT_FLOW_FILE: &str = "flot.toml";

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

#[cfg(test)]
mod tests;
