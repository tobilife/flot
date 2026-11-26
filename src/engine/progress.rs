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

/// A small flow run twice with a sink: every event it reports, in order.
/// Its steps are shell commands, so it runs where `sh` is the shell.
#[cfg(all(test, unix))]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::core::flow::Flow;
    use crate::engine::runner::{RunOptions, Runner};
    use crate::store::{HostSecret, Store};

    /// `pass` prints lines ending in `\n` and `\r\n` and a last one ending
    /// in neither, and its proof prints one; `fail` prints to stderr and
    /// fails; `after`, which needs it, is not run.
    const FLOW: &str = r#"
[flow]
name = "progress"

[[step]]
id = "pass"
run = "printf 'one\ntwo\r\n\nthree'"
claims = ["it passes"]
verify = "echo checked"

[[step]]
id = "fail"
run = "echo broken >&2; exit 1"

[[step]]
id = "after"
needs = ["fail"]
run = "true"
claims = ["it ran"]
verify = "true"
"#;

    /// The events of one run, with the durations, which differ from one
    /// run to the next, zeroed.
    fn events_of(runner: &Runner<'_>) -> Vec<Event> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&seen);
        let options = RunOptions {
            progress: Sink::new(move |event| kept.lock().unwrap().push(event.clone())),
            ..RunOptions::default()
        };
        runner.run(&options).unwrap();
        let events = seen.lock().unwrap().clone();
        events
            .into_iter()
            .map(|event| match event {
                Event::StepFinished {
                    step,
                    attempt,
                    candidate,
                    status,
                    claims,
                    ..
                } => Event::StepFinished {
                    step,
                    attempt,
                    candidate,
                    status,
                    duration_ms: 0,
                    claims,
                },
                Event::RunFinished { .. } => Event::RunFinished { duration_ms: 0 },
                other => other,
            })
            .collect()
    }

    #[test]
    fn a_run_reports_each_step_as_it_settles() {
        let dir = tempfile::tempdir().unwrap();
        let flow = Flow::parse(FLOW).unwrap();
        let store = Store::open(dir.path(), HostSecret::from_bytes([7; 32])).unwrap();
        let runner = Runner::new(&flow, dir.path(), &store);
        let claims = |verified, unverified| ClaimCounts {
            verified,
            unverified,
            refuted: 0,
        };
        let started = |step: &str| Event::StepStarted {
            step: step.into(),
            attempt: 1,
            attempts: 1,
            candidate: None,
        };
        let output = |step: &str, source, line: &str| Event::Output {
            step: step.into(),
            source,
            line: line.into(),
        };
        let finished = |step: &str, attempt, status, claims| Event::StepFinished {
            step: step.into(),
            attempt,
            candidate: None,
            status,
            duration_ms: 0,
            claims,
        };
        let run = Event::RunStarted { steps: 3, jobs: 1 };
        let cancelled = finished("after", 0, StepStatus::Cancelled, claims(0, 1));
        let failing = [
            started("fail"),
            output("fail", Source::Stderr, "broken"),
            finished("fail", 1, StepStatus::Failed, claims(0, 0)),
        ];

        let mut first = vec![run.clone()];
        first.extend(failing.iter().cloned());
        first.extend([
            started("pass"),
            output("pass", Source::Stdout, "one"),
            output("pass", Source::Stdout, "two"),
            output("pass", Source::Stdout, ""),
            output("pass", Source::Stdout, "three"),
            output("pass", Source::Proof, "checked"),
            finished("pass", 1, StepStatus::Ok, claims(1, 0)),
            cancelled.clone(),
            Event::RunFinished { duration_ms: 0 },
        ]);
        assert_eq!(events_of(&runner), first);

        // A failure is never reused, so `fail` runs again; `pass` is reused.
        let mut second = vec![run];
        second.extend(failing);
        second.extend([
            Event::StepCached {
                step: "pass".into(),
                candidate: None,
                claims: claims(1, 0),
            },
            cancelled,
            Event::RunFinished { duration_ms: 0 },
        ]);
        assert_eq!(events_of(&runner), second);
    }
}
