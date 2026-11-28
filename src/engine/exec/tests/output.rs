//! Tests of a command's output: its byte budget, the lines told as they are
//! kept, and more than a pipe holds.

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
