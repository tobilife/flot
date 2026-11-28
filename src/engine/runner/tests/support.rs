//! A flow in a temporary workspace, with a store of its own there.

use std::path::Path;

use crate::core::flow::Flow;
use crate::core::receipt::Receipt;
use crate::engine::runner::{RunOptions, RunReport, Runner};
use crate::store::{HostSecret, Store};

pub(super) struct Workspace {
    dir: tempfile::TempDir,
    pub(super) flow: Flow,
    pub(super) store: Store,
}

impl Workspace {
    pub(super) fn new(flow: &str) -> Workspace {
        let dir = tempfile::tempdir().unwrap();
        let flow = Flow::parse(flow).unwrap();
        let store = Store::open(dir.path(), HostSecret::from_bytes([7; 32])).unwrap();
        Workspace { dir, flow, store }
    }

    pub(super) fn root(&self) -> &Path {
        self.dir.path()
    }

    pub(super) fn write(&self, path: &str, text: &str) {
        std::fs::write(self.root().join(path), text).unwrap();
    }

    pub(super) fn runner(&self) -> Runner<'_> {
        Runner::new(&self.flow, self.root(), &self.store)
    }

    pub(super) fn run(&self, options: &RunOptions) -> RunReport {
        self.runner().run(options).unwrap()
    }

    /// The receipt `report` settled `step` with.
    pub(super) fn receipt(&self, report: &RunReport, step: &str) -> Receipt {
        let entry = report
            .steps
            .iter()
            .find(|entry| entry.step_id == step)
            .unwrap();
        self.store.load_receipt(&entry.receipt_id).unwrap()
    }
}

/// A run that reuses nothing.
pub(super) fn forced() -> RunOptions {
    RunOptions {
        force: true,
        ..RunOptions::default()
    }
}
