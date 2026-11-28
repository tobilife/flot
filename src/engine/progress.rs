//! What a run reports while it happens: steps starting, the lines they
//! print, and how each ended. The runner hands these to a [`Sink`] and never
//! renders them; nothing here reaches a receipt or a manifest.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::core::receipt::{ClaimCounts, Receipt, StepStatus};
use crate::engine::exec;

/// One thing that happened during a run, in the order it happened. Steps
/// run in parallel report interleaved, each line whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The run is about to settle `steps` steps, at most `jobs` at a time.
    RunStarted { steps: usize, jobs: usize },
    /// An attempt at a step starts executing: `attempt` of the `attempts`
    /// it may take, as `candidate` when the step names candidates.
    StepStarted {
        step: String,
        attempt: u32,
        attempts: u32,
        candidate: Option<String>,
    },
    /// A line a step's command, or its proof, printed, without its line
    /// ending.
    Output {
        step: String,
        source: Source,
        line: String,
    },
    /// A step is settled by a receipt recorded earlier, without running.
    StepCached {
        step: String,
        candidate: Option<String>,
        claims: ClaimCounts,
    },
    /// An attempt at a step finished, with its proof; or, at `attempt` 0, a
    /// step was settled without executing: cancelled, or unable to start.
    StepFinished {
        step: String,
        attempt: u32,
        candidate: Option<String>,
        status: StepStatus,
        duration_ms: u64,
        claims: ClaimCounts,
        /// Why a step that never executed was not run, as its receipt says:
        /// `not run: dependency lint (failed) did not hold`.
        why: Option<String>,
    },
    /// Every step is settled; the run is recorded next.
    RunFinished { duration_ms: u64 },
}

/// Where a line of output came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Stdout,
    Stderr,
    /// The step's proof, either stream: its output is evidence as one.
    Proof,
}

impl Event {
    /// The end of an attempt at a step, or at `attempt` 0 of a step that
    /// never executed, as `receipt` records it.
    pub fn finished(receipt: &Receipt, attempt: u32, took: Duration) -> Event {
        Event::StepFinished {
            step: receipt.identity.step_id.clone(),
            attempt,
            candidate: receipt.identity.candidate.clone(),
            status: receipt.identity.status,
            duration_ms: exec::millis(took),
            claims: receipt.claim_counts(),
            why: receipt
                .identity
                .limitations
                .iter()
                .find(|limitation| {
                    limitation.starts_with("not run: ") || limitation.starts_with("could not run: ")
                })
                .cloned(),
        }
    }
}

type Listener = Arc<dyn Fn(&Event) + Send + Sync>;

/// Where a run's events go; by default, nowhere. Steps run on several
/// threads at once, and each calls it from its own, lines from the threads
/// that read the step's output: a listener that blocks holds the step up, so
/// it should hand events on, not show them itself.
#[derive(Clone, Default)]
pub struct Sink(Option<Listener>);

impl Sink {
    pub fn new(listener: impl Fn(&Event) + Send + Sync + 'static) -> Sink {
        Sink(Some(Arc::new(listener)))
    }

    /// Report the event `event` makes, made only when someone listens.
    pub fn emit(&self, event: impl FnOnce() -> Event) {
        if let Some(listener) = &self.0 {
            listener(&event());
        }
    }

    /// The lines a command of `step` prints, reported as its own output or,
    /// with `proof`, as its proof's. `None` when no one listens.
    pub(crate) fn lines(&self, step: &str, proof: bool) -> Option<exec::Lines> {
        let listener = Arc::clone(self.0.as_ref()?);
        let step = step.to_string();
        Some(Arc::new(move |stream, line: &[u8]| {
            let source = match (proof, stream) {
                (true, _) => Source::Proof,
                (false, exec::Stream::Stdout) => Source::Stdout,
                (false, exec::Stream::Stderr) => Source::Stderr,
            };
            listener(&Event::Output {
                step: step.clone(),
                source,
                line: String::from_utf8_lossy(line).into_owned(),
            });
        }))
    }
}

impl fmt::Debug for Sink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = if self.0.is_some() {
            "listening"
        } else {
            "none"
        };
        f.debug_tuple("Sink").field(&state).finish()
    }
}

#[cfg(all(test, unix))]
mod tests;
