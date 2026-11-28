//! Tests of the live and plain views.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::core::receipt::{ClaimCounts, StepStatus};
use crate::service::progress::{Event, Source};

use super::super::terminal_safe;
use super::live::{show_live, Live, ERASE_ROW};
use super::plain::Plain;
use super::Message;

fn started(step: &str, attempt: u32, attempts: u32, candidate: Option<&str>) -> Event {
    Event::StepStarted {
        step: step.into(),
        attempt,
        attempts,
        candidate: candidate.map(str::to_string),
    }
}

fn output(step: &str, source: Source, line: &str) -> Event {
    Event::Output {
        step: step.into(),
        source,
        line: line.into(),
    }
}

fn finished(
    step: &str,
    attempt: u32,
    status: StepStatus,
    millis: u64,
    claims: ClaimCounts,
) -> Event {
    Event::StepFinished {
        step: step.into(),
        attempt,
        candidate: None,
        status,
        duration_ms: millis,
        claims,
        why: None,
    }
}

fn claims(verified: usize, unverified: usize, refuted: usize) -> ClaimCounts {
    ClaimCounts {
        verified,
        unverified,
        refuted,
    }
}

/// Two steps running at once, printing in turns: each frame takes back
/// exactly the rows the last one drew, every line arrives whole, and the
/// view ends with its rows taken back.
#[test]
fn the_live_view_takes_back_only_its_own_rows_and_keeps_lines_whole() {
    let start = Instant::now();
    let at = |millis| start + Duration::from_millis(millis);
    let mut live = Live::new(Vec::new(), false);
    live.event(&started("build", 1, 1, None), start);
    live.event(&started("test", 1, 3, None), start);
    live.draw(at(1200), 80).unwrap();
    live.event(&output("build", Source::Stdout, "compiling a"), at(1300));
    live.event(&output("test", Source::Stderr, "warning: slow"), at(1300));
    live.event(&output("build", Source::Stdout, "compiling b"), at(1400));
    live.draw(at(1500), 80).unwrap();
    live.event(&output("build", Source::Proof, "ok"), at(1600));
    live.event(
        &finished("build", 1, StepStatus::Ok, 1600, claims(2, 0, 0)),
        at(1600),
    );
    live.draw(at(2000), 80).unwrap();
    // Too narrow for its row: the row is cut, never wrapped.
    live.draw(at(2100), 12).unwrap();
    live.event(
        &finished("test", 1, StepStatus::Failed, 2200, claims(0, 1, 0)),
        at(2200),
    );
    live.clear().unwrap();

    let erase = |rows: usize| ERASE_ROW.repeat(rows);
    let expected = [
        "  build running 1.2s\n  test running 1.2s attempt 1/3\n".to_string(),
        erase(2),
        "[build] compiling a\n[test:stderr] warning: slow\n[build] compiling b\n".into(),
        "  build running 1.5s\n  test running 1.5s attempt 1/3\n".into(),
        erase(2),
        "[build:proof] ok\n\u{2713} build passed 1.6s claims 2/2\n".into(),
        "  test running 2.0s attempt 1/3\n".into(),
        erase(1),
        "  test runn\n".into(),
        erase(1),
        "\u{2717} test failed 2.2s claims 0/1 attempt 1/3\n".into(),
    ]
    .concat();
    let written = String::from_utf8(live.out).unwrap();
    assert_eq!(written, expected);
    assert_eq!(written.matches("\x1b[A").count(), 6);
    assert_eq!(written.matches("\x1b[K").count(), 6);
    // Nothing but rows going up and being cleared: no hidden cursor, no
    // other screen.
    assert_eq!(written.matches('\x1b').count(), 12);
}

#[test]
fn the_live_view_paints_only_when_asked() {
    let mut live = Live::new(Vec::new(), true);
    live.event(&output("build", Source::Stderr, "careful"), Instant::now());
    live.event(
        &finished("build", 1, StepStatus::Ok, 0, claims(0, 0, 0)),
        Instant::now(),
    );
    live.clear().unwrap();
    let written = String::from_utf8(live.out).unwrap();
    assert_eq!(
        written,
        "\x1b[33m[build:stderr]\x1b[0m careful\n\x1b[32m\u{2713}\x1b[0m build passed 0ms\n"
    );
}

/// A step's output can carry escape sequences of its own; they are shown,
/// not acted on.
#[test]
fn a_step_cannot_drive_the_terminal_through_the_view() {
    let mut live = Live::new(Vec::new(), false);
    live.event(
        &output("build", Source::Stdout, "\x1b[2Jgone\r"),
        Instant::now(),
    );
    live.clear().unwrap();
    assert_eq!(
        String::from_utf8(live.out).unwrap(),
        "[build] \\u{1b}[2Jgone\\u{d}\n"
    );
    let mut plain = Plain::default();
    let line = plain.line(&output("build", Source::Stdout, "\x1b[2Jgone"));
    assert_eq!(terminal_safe(&line), "[build] \\u{1b}[2Jgone");
}

#[test]
fn the_plain_view_says_which_attempt_and_candidate_each_line_is() {
    let mut plain = Plain::default();
    let lines: Vec<String> = [
        Event::RunStarted { steps: 1, jobs: 2 },
        started("flaky", 1, 2, None),
        finished("flaky", 1, StepStatus::Failed, 120, claims(0, 1, 0)),
        started("flaky", 2, 2, None),
        finished("flaky", 2, StepStatus::Ok, 1999, claims(1, 0, 0)),
        started("pick", 1, 1, Some("fast")),
        Event::StepFinished {
            step: "pick".into(),
            attempt: 1,
            candidate: Some("fast".into()),
            status: StepStatus::Ok,
            duration_ms: 50,
            claims: claims(0, 0, 1),
            why: None,
        },
        Event::StepCached {
            step: "lint".into(),
            candidate: None,
            claims: claims(1, 0, 0),
        },
        Event::StepFinished {
            step: "ship".into(),
            attempt: 0,
            candidate: None,
            status: StepStatus::Cancelled,
            duration_ms: 0,
            claims: claims(0, 1, 0),
            why: Some("not run: dependency lint (failed) did not hold".into()),
        },
        Event::RunFinished { duration_ms: 12345 },
    ]
    .iter()
    .map(|event| plain.line(event))
    .collect();
    assert_eq!(
        lines,
        [
            "run 1 step, 2 jobs",
            "start flaky attempt 1/2",
            "done flaky failed 0.1s claims 0/1 attempt 1/2",
            "start flaky attempt 2/2",
            "done flaky passed 1.9s claims 1/1 attempt 2/2",
            "start pick candidate fast",
            "done pick refuted 50ms claims 0/1 candidate fast",
            "cached lint passed claims 1/1",
            "done ship not run: dependency lint (failed) did not hold",
            "run finished 12.3s",
        ]
    );
}

/// A step that never executed took no time, and attempt 0 is no attempt:
/// neither is told, even of a step whose other candidate may take three.
#[test]
fn the_plain_view_tells_no_time_or_attempt_of_a_step_that_never_executed() {
    let mut plain = Plain::default();
    let unrun = |candidate: &str, why: Option<&str>| Event::StepFinished {
        step: "solve".into(),
        attempt: 0,
        candidate: Some(candidate.into()),
        status: StepStatus::Cancelled,
        duration_ms: 0,
        claims: claims(0, 1, 0),
        why: why.map(str::to_string),
    };
    let lines: Vec<String> = [
        started("solve", 1, 3, Some("wrong")),
        unrun(
            "cheap",
            Some("could not run: its workspace could not be prepared"),
        ),
        unrun("pricey", None),
    ]
    .iter()
    .map(|event| plain.line(event))
    .collect();
    assert_eq!(
        lines,
        [
            "start solve attempt 1/3 candidate wrong",
            "done solve could not run: its workspace could not be prepared candidate cheap",
            "done solve cancelled claims 0/1 candidate pricey",
        ]
    );
}

/// A terminal that keeps each write apart.
struct Writes<'w>(&'w mut Vec<String>);

impl std::io::Write for Writes<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.push(String::from_utf8(bytes.to_vec()).unwrap());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Lines that arrive faster than the view redraws are written 256 events at
/// a time: neither one write per line, nor one for all of them.
#[test]
fn the_live_view_writes_what_it_takes_in_batches_of_256_events() {
    let (sender, receiver) = mpsc::channel();
    for number in 0..300 {
        let line = output("build", Source::Stdout, &number.to_string());
        sender.send(Message::Event(line)).unwrap();
    }
    sender.send(Message::Done).unwrap();
    let mut writes = Vec::new();
    show_live(&receiver, Live::new(Writes(&mut writes), false));
    let lines = |numbers: std::ops::Range<usize>| -> String {
        numbers.map(|n| format!("[build] {n}\n")).collect()
    };
    assert_eq!(writes, [lines(0..256), lines(256..300)]);
}
