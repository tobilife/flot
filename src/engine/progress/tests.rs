//! A small flow run twice with a sink: every event it reports, in order.
//! Its steps are shell commands, so it runs where `sh` is the shell.

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
                why,
                ..
            } => Event::StepFinished {
                step,
                attempt,
                candidate,
                status,
                duration_ms: 0,
                claims,
                why,
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
        why: None,
    };
    let run = Event::RunStarted { steps: 3, jobs: 1 };
    let cancelled = Event::StepFinished {
        step: "after".into(),
        attempt: 0,
        candidate: None,
        status: StepStatus::Cancelled,
        duration_ms: 0,
        claims: claims(0, 1),
        why: Some("not run: dependency fail (failed) did not hold".into()),
    };
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

#[test]
fn a_step_that_never_ran_says_why_and_one_that_ran_does_not() {
    use crate::core::receipt::{Observed, ReceiptIdentity, ToolIdentity};
    let receipt = |status, limitations: &[&str]| {
        Receipt::seal(
            ReceiptIdentity {
                flow: "f".into(),
                step_id: "s".into(),
                step_key: "sha256:k".into(),
                command: "true".into(),
                status,
                exit_code: None,
                stdout_digest: String::new(),
                stderr_digest: String::new(),
                inputs: vec![],
                verify_inputs: vec![],
                outputs: vec![],
                changed: Default::default(),
                deps: vec![],
                claims: vec![],
                limitations: limitations.iter().map(|l| l.to_string()).collect(),
                tool: ToolIdentity::default(),
                candidate: None,
                sandbox: None,
            },
            Observed::default(),
        )
    };
    let why = |receipt: &Receipt| match Event::finished(receipt, 0, Duration::ZERO) {
        Event::StepFinished { why, .. } => why,
        other => panic!("{other:?}"),
    };
    let sandbox = "could not run: its sandbox cannot be enforced here: no landlock";
    assert_eq!(
        why(&receipt(
            StepStatus::Failed,
            &["step declares no inputs", sandbox]
        )),
        Some(sandbox.to_string())
    );
    assert_eq!(
        why(&receipt(
            StepStatus::Cancelled,
            &["not run: flot was asked to stop"]
        )),
        Some("not run: flot was asked to stop".to_string())
    );
    // A candidate left out is not why the step did not run.
    assert_eq!(
        why(&receipt(
            StepStatus::Ok,
            &["candidate b not run: run cost ceiling of $1 reached"]
        )),
        None
    );
}
