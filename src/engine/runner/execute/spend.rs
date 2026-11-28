//! The file a command reports its spend in: a fresh `$FLOT_METRICS` path
//! for each command, read back and removed once it is done.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use anyhow::Result;

use crate::core::flow::Step;
use crate::core::metrics::{self, Metrics};
use crate::engine::runner::{Runner, METRICS_SEQUENCE};

impl Runner<'_> {
    /// The step's environment plus a fresh `$FLOT_METRICS` path for one
    /// command to report its spend in.
    pub(super) fn metered_env(&self, step: &Step) -> Result<(BTreeMap<String, String>, PathBuf)> {
        let mut env = self.step_env(step);
        let metrics_path = self.store.root().join("tmp").join(format!(
            "metrics-{}-{}.json",
            std::process::id(),
            METRICS_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(metrics_path.parent().expect("tmp has a parent"))?;
        let _ = std::fs::remove_file(&metrics_path);
        env.insert(
            "FLOT_METRICS".into(),
            metrics_path.to_string_lossy().to_string(),
        );
        Ok((env, metrics_path))
    }
}

/// Largest metrics file read. The file is a handful of numbers; anything
/// bigger is not one.
const MAX_METRICS_BYTES: u64 = 64 * 1024;

/// Read and remove one step's metrics file. Absence is not an error: most steps
/// do not bill anything. Anything else that is not a small, regular metrics
/// file is reported and treated as silent: the path is the step's to write, so
/// it may hold a pipe or a link to a device that never ends.
pub(super) fn read_metrics(path: &Path) -> (Option<Metrics>, Vec<String>) {
    let ignored = |why: String| {
        let _ = std::fs::remove_file(path);
        (
            None,
            vec![format!(
                "metrics file ignored: {why}; this step's spend is unknown"
            )],
        )
    };
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (None, Vec::new()),
        Err(error) => return ignored(format!("cannot inspect it ({error})")),
    };
    if !meta.file_type().is_file() {
        return ignored("it is not a regular file".into());
    }
    if meta.len() > MAX_METRICS_BYTES {
        return ignored(format!("it is larger than {MAX_METRICS_BYTES} bytes"));
    }
    let text = match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return ignored("it is not UTF-8".into()),
        },
        Err(error) => return ignored(format!("cannot read it ({error})")),
    };
    let _ = std::fs::remove_file(path);
    match metrics::parse(&text) {
        Ok((metrics, notes)) => (Some(metrics), notes),
        Err(error) => (
            None,
            vec![format!(
                "metrics file ignored: {error:#}; this step's spend is unknown"
            )],
        ),
    }
}
