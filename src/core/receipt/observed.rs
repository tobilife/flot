//! What one execution of a step was seen to do: its timing, host and
//! output previews. Never hashed.

use serde::{Deserialize, Serialize};

use crate::core::metrics::Metrics;

impl Observed {
    /// Whether the inline previews and byte counts describe these complete
    /// streams. Previews are what a reader sees first, so a preview that does
    /// not come from the stored output is a forgery, whatever signs it.
    /// A cut preview is a beginning of the stream, or a beginning, the
    /// [`OMITTED`] marker and an end of it: nothing in it was not printed.
    pub fn describes(&self, stdout: &[u8], stderr: &[u8]) -> bool {
        fn preview_of(inline: &str, truncated: bool, bytes: usize, data: &[u8]) -> bool {
            let whole = String::from_utf8_lossy(data);
            let cut = |head: &str, tail: &str| {
                head.len() + tail.len() < whole.len()
                    && whole.starts_with(head)
                    && whole.ends_with(tail)
            };
            bytes == data.len()
                && if truncated {
                    cut(inline, "")
                        || inline
                            .match_indices(OMITTED)
                            .any(|(at, _)| cut(&inline[..at], &inline[at + OMITTED.len()..]))
                } else {
                    inline == whole
                }
        }
        preview_of(
            &self.stdout_inline,
            self.stdout_truncated,
            self.stdout_bytes,
            stdout,
        ) && preview_of(
            &self.stderr_inline,
            self.stderr_truncated,
            self.stderr_bytes,
            stderr,
        )
    }
}

/// Stands where a stream preview leaves out the middle of the output.
pub const OMITTED: &str = "\n[... output omitted here; the complete stream is in the store ...]\n";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Observed {
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub host_os: String,
    /// Bounded preview of stdout. The complete stream stays in the object store
    /// under `stdout_digest`.
    pub stdout_inline: String,
    pub stdout_truncated: bool,
    pub stderr_inline: String,
    pub stderr_truncated: bool,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    /// What the step reported through `$FLOT_METRICS`; `None` when it reported
    /// nothing or did not execute.
    #[serde(default)]
    pub metrics: Option<Metrics>,
    /// Problems with the metrics file. Kept out of the hashed body, because
    /// what a step bills is an observation, not part of the work.
    #[serde(default)]
    pub metrics_notes: Vec<String>,
}
