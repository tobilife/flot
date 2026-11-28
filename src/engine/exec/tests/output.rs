//! Tests of a command's output: its byte budget, the lines told as they are
//! kept, and more than a pipe holds.

use std::io::Read;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::Instant;

use super::*;

#[test]
fn a_stream_past_its_budget_stops_the_command() {
    let started = Instant::now();
    let outcome = run_command(
        "yes",
        Path::new("."),
        &env(),
        Duration::from_secs(30),
        4096,
        &StopSignal::default(),
    )
    .unwrap();
    assert!(outcome.output_overflow);
    assert!(!outcome.succeeded());
    assert_eq!(outcome.stdout.len(), 4096);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn lines_are_told_as_they_are_kept_and_stop_where_the_record_is_cut() {
    let told = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&told);
    let lines: Lines = Arc::new(move |stream, line: &[u8]| {
        let line = String::from_utf8_lossy(line).into_owned();
        seen.lock().unwrap().push((stream, line));
    });
    // 14 bytes on stdout against a budget of 9.
    let run = |lines: Option<&Lines>| {
        run_confined(
            "printf 'ab\\ncd\\r\\n' >&2; printf 'one\\ntwo\\nthree\\n'",
            Path::new("."),
            &env(),
            Limits {
                timeout: Duration::from_secs(10),
                max_output_bytes: 9,
            },
            &StopSignal::default(),
            None,
            lines,
        )
        .unwrap()
    };
    let quiet = run(None);
    let outcome = run(Some(&lines));
    assert_eq!(outcome.stdout, b"one\ntwo\nt");
    assert_eq!(outcome.stderr, b"ab\ncd\r\n");
    assert_eq!(
        (&outcome.stdout, &outcome.stderr),
        (&quiet.stdout, &quiet.stderr)
    );
    let told = told.lock().unwrap();
    let of = |stream| {
        told.iter()
            .filter(|(from, _)| *from == stream)
            .map(|(_, line)| line.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(of(Stream::Stdout), ["one", "two", "t"]);
    assert_eq!(of(Stream::Stderr), ["ab", "cd"]);
}

#[test]
fn large_output_does_not_deadlock() {
    let outcome = run("yes flot | head -c 300000");
    assert_eq!(outcome.stdout.len(), 300_000);
    assert!(outcome.succeeded());
}

#[test]
fn a_stream_that_fills_its_budget_exactly_is_kept_whole() {
    // Still running once it has printed, so a budget wrongly passed would
    // stop it.
    let outcome = run_command(
        "printf abcd; sleep 1",
        Path::new("."),
        &env(),
        Duration::from_secs(10),
        4,
        &StopSignal::default(),
    )
    .unwrap();
    assert_eq!(outcome.stdout, b"abcd");
    assert!(
        !outcome.output_overflow && outcome.succeeded(),
        "{outcome:?}"
    );
}

/// What a step prints while a process flot neither started nor can stop
/// holds its stdout open for `hold` seconds after it ends, and how long
/// the step took. The holder opens the pipe through /proc.
#[cfg(target_os = "linux")]
fn held_open_by_another(hold: &str) -> (Outcome, Duration) {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().display();
    let mut holder = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "while [ ! -s {d}/fd ]; do sleep 0.01; done; exec 3>\"$(cat {d}/fd)\"; \
             echo late >&3; touch {d}/held; sleep {hold}"
        ))
        .spawn()
        .unwrap();
    let started = Instant::now();
    let outcome = run(&format!(
        "echo /proc/$$/fd/1 > {d}/fd; while [ ! -e {d}/held ]; do sleep 0.01; done"
    ));
    let took = started.elapsed();
    holder.wait().unwrap();
    (outcome, took)
}

#[cfg(target_os = "linux")]
#[test]
fn output_is_waited_for_a_while_and_no_longer() {
    // Closed within the grace: the stream is whole.
    let (outcome, took) = held_open_by_another("0.5");
    assert_eq!(outcome.stdout, b"late\n");
    assert!(!outcome.output_detached, "{outcome:?}");
    assert!(took >= Duration::from_millis(500), "{took:?}");
    // Held past it: stdout is cut off there, though stderr closed.
    let (outcome, took) = held_open_by_another("4");
    assert_eq!(outcome.stdout, b"late\n");
    assert!(outcome.output_detached, "{outcome:?}");
    assert!(took < Duration::from_millis(3800), "{took:?}");
}

/// A stream whose reads go as listed, and then end.
struct Reads(Vec<std::io::Result<&'static [u8]>>);

impl Read for Reads {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.0.is_empty() {
            return Ok(0);
        }
        let bytes = self.0.remove(0)?;
        buf[..bytes.len()].copy_from_slice(bytes);
        Ok(bytes.len())
    }
}

#[test]
fn an_interrupted_read_is_retried_and_a_failed_one_ends_the_stream() {
    use std::io::{Error, ErrorKind};
    let reads = Reads(vec![
        Err(Error::from(ErrorKind::Interrupted)),
        Ok(b"ab"),
        Err(Error::from(ErrorKind::BrokenPipe)),
        Ok(b"cd"),
    ]);
    let overflow = Arc::new(AtomicBool::new(false));
    let (kept, complete) = super::super::output::drain(Some(reads), CAP, overflow, None)
        .collect(Instant::now() + Duration::from_secs(10));
    assert_eq!((kept.as_slice(), complete), (&b"ab"[..], true));
}
