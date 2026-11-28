//! Tests of running one command: what it reads, prints and exits with, and
//! the helpers the exec tests share.

use std::collections::BTreeMap;
use std::path::Path;

use super::*;

mod output;
mod strays;

pub(super) const CAP: usize = 1 << 20;

pub(super) fn env() -> BTreeMap<String, String> {
    BTreeMap::new()
}

pub(super) fn run(command: &str) -> Outcome {
    run_command(
        command,
        Path::new("."),
        &env(),
        Duration::from_secs(10),
        CAP,
        &StopSignal::default(),
    )
    .unwrap()
}

#[test]
fn captures_stdout_and_exit_code() {
    let outcome = run("printf 'hi'");
    assert_eq!(outcome.stdout, b"hi");
    assert_eq!(outcome.exit_code, Some(0));
    assert!(outcome.succeeded());
}

#[test]
fn captures_stderr_separately_from_stdout() {
    let outcome = run("printf 'out'; printf 'err' >&2");
    assert_eq!(outcome.stdout, b"out");
    assert_eq!(outcome.stderr, b"err");
}

#[test]
fn reports_nonzero_exit_without_claiming_success() {
    let outcome = run("exit 3");
    assert_eq!(outcome.exit_code, Some(3));
    assert!(!outcome.succeeded());
}

#[test]
fn declared_environment_reaches_the_command() {
    let mut env = env();
    env.insert("FLOT_TEST_VALUE".into(), "present".into());
    let outcome = run_command(
        "printf '%s' \"$FLOT_TEST_VALUE\"",
        Path::new("."),
        &env,
        Duration::from_secs(10),
        CAP,
        &StopSignal::default(),
    )
    .unwrap();
    assert_eq!(outcome.stdout, b"present");
}

#[test]
fn runs_in_the_requested_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("marker"), "x").unwrap();
    let outcome = run_command(
        "ls",
        dir.path(),
        &env(),
        Duration::from_secs(10),
        CAP,
        &StopSignal::default(),
    )
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&outcome.stdout).trim(), "marker");
}

#[test]
fn a_step_over_its_budget_is_stopped_and_marked_timed_out() {
    let outcome = run_command(
        "sleep 30",
        Path::new("."),
        &env(),
        Duration::from_millis(150),
        CAP,
        &StopSignal::default(),
    )
    .unwrap();
    assert!(outcome.timed_out);
    assert!(!outcome.succeeded());
    assert!(outcome.duration_ms < 10_000, "{}ms", outcome.duration_ms);
}

#[test]
fn a_failing_line_fails_a_multi_line_command() {
    let outcome = run("false\necho after");
    assert_eq!(outcome.exit_code, Some(1));
    assert!(outcome.stdout.is_empty(), "the script stops at the failure");
    // Conditions remain the author's to handle.
    assert!(run("false || true\necho ok").succeeded());
}

// Windows has no signals: a killed process exits with whatever code its
// killer chose.
#[cfg(unix)]
#[test]
fn a_signal_death_is_reported_as_a_signal() {
    let outcome = run("kill -9 $$");
    assert_eq!(outcome.exit_code, None);
    assert_eq!(outcome.signal, Some(9));
}
