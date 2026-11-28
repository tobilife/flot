//! The runner's tests: a run it refuses, and the runner end to end.

use super::{RunOptions, Runner};
use crate::core::flow::Flow;
use crate::store::{HostSecret, Store};

#[test]
fn zero_jobs_is_refused_before_anything_runs() {
    let dir = tempfile::tempdir().unwrap();
    let flow =
        Flow::parse("[flow]\nname = \"j\"\n\n[[step]]\nid = \"s\"\nrun = \"true\"\n").unwrap();
    let store = Store::open(dir.path(), HostSecret::from_bytes([7; 32])).unwrap();
    let error = Runner::new(&flow, dir.path(), &store)
        .run(&RunOptions {
            jobs: Some(0),
            ..RunOptions::default()
        })
        .unwrap_err();
    assert!(error.to_string().contains("at least 1"), "{error}");
    assert!(store.list_runs().unwrap().is_empty());
}

/// The runner end to end, pinned: which steps run, what each receipt,
/// manifest and cache entry then holds, byte for byte. Its steps are
/// shell commands, so it runs where `sh` is the shell.
#[cfg(unix)]
mod scenario;

/// A flow in a workspace and a store of its own, for the tests below. They
/// run shell commands too.
#[cfg(unix)]
mod support;

/// Running a step's command: one that cannot start, a stray tolerated, and
/// outputs the store cannot take.
#[cfg(unix)]
mod execution;

/// What a step's candidates leave in the workspace and their receipts.
#[cfg(unix)]
mod candidates;

/// What a run counts as spent: its proofs', and against its ceiling.
#[cfg(unix)]
mod spend;

/// What a plan predicts: what is reused, and what each step reads.
#[cfg(unix)]
mod plan;

/// What a run takes from the runs before it.
#[cfg(unix)]
mod earlier;
