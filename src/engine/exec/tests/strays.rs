//! Tests of what a command leaves running: background processes in its
//! group, and processes that leave the group.

use std::time::Instant;

use super::*;

#[test]
fn background_processes_are_stopped_when_the_step_exits() {
    let started = Instant::now();
    let outcome = run("echo started; sleep 30 &");
    assert!(outcome.succeeded());
    assert!(outcome.strays_stopped);
    assert!(!outcome.output_detached);
    assert_eq!(outcome.stdout, b"started\n");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(!run("echo alone").strays_stopped);
}

#[test]
fn a_process_that_leaves_the_group_cannot_hold_the_step_open() {
    let started = Instant::now();
    // perl's POSIX::setsid, because macOS ships no `setsid` command. The
    // step waits until the escapee has left its group, so the group kill
    // cannot reach it first.
    let outcome = run(concat!(
        "d=$(mktemp -d); ",
        "perl -MPOSIX -e 'setsid(); open(my $f, \">\", shift) or die; close $f; exec @ARGV' ",
        "\"$d/left\" sleep 30 & ",
        "while [ ! -e \"$d/left\" ]; do sleep 0.01; done; rm -r \"$d\"; echo early",
    ));
    // A new session leaves a POSIX process group, out of reach of the
    // group kill. On Linux flot adopts the escapee once its parent exits
    // and stops it, and on macOS it recorded the escapee below the step
    // and stops it; on other systems nothing can, and flot only stops
    // waiting for it. A Windows job cannot be left: the escapee is
    // stopped with the rest.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert!(
        !outcome.output_detached && outcome.escaped.len() == 1,
        "{outcome:?}"
    );
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    assert!(outcome.output_detached);
    #[cfg(windows)]
    assert!(
        !outcome.output_detached && outcome.strays_stopped,
        "{outcome:?}"
    );
    assert_eq!(outcome.stdout, b"early\n");
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
}

/// `command` with `setsid sh` as this system runs it: macOS ships no
/// `setsid` command, so perl's POSIX::setsid stands in.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn with_setsid(command: &str) -> String {
    if cfg!(target_os = "macos") {
        command.replace(
            "setsid sh",
            "/usr/bin/perl -MPOSIX -e 'setsid(); exec @ARGV' /bin/sh",
        )
    } else {
        command.to_owned()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_process_that_leaves_the_group_is_stopped_before_it_can_write() {
    let dir = tempfile::tempdir().unwrap();
    // Its output goes nowhere, so no pipe held open gives it away.
    let outcome = run_command(
        &with_setsid(concat!(
            "setsid sh -c 'touch left; sleep 1; echo late > late' >/dev/null 2>&1 &\n",
            "while [ ! -e left ]; do sleep 0.01; done",
        )),
        dir.path(),
        &env(),
        Duration::from_secs(10),
        CAP,
        &StopSignal::default(),
    )
    .unwrap();
    assert!(outcome.succeeded());
    assert_eq!(outcome.escaped.len(), 1, "{outcome:?}");
    assert!(outcome.escaped[0].traced, "it kept the command's token");
    assert!(!outcome.strays_stopped, "it left the group: not a stray");
    assert!(!outcome.output_detached);
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !dir.path().join("late").exists(),
        "the escapee outlived the step"
    );
    // What stays in the group and ends with it escaped nothing.
    assert!(run("sleep 0.01 & wait").escaped.is_empty());
    assert!(run("sleep 30 &").escaped.is_empty());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_daemon_whose_parent_exited_at_once_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    // The shell between the step and the daemon exits as soon as it has
    // started it, so the daemon has no parent left in the step long
    // before the step ends; it still carries the step's token.
    let outcome = run_command(
        &with_setsid(concat!(
            "sh -c \"setsid sh -c 'touch left; sleep 1; echo late > late' &\" ",
            ">/dev/null 2>&1\n",
            "while [ ! -e left ]; do sleep 0.01; done",
        )),
        dir.path(),
        &env(),
        Duration::from_secs(10),
        CAP,
        &StopSignal::default(),
    )
    .unwrap();
    assert!(outcome.succeeded());
    assert_eq!(outcome.escaped.len(), 1, "{outcome:?}");
    assert!(outcome.escaped[0].traced, "it kept the command's token");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !dir.path().join("late").exists(),
        "the daemon outlived the step"
    );
}

#[test]
fn a_command_flot_stopped_left_no_strays_of_its_own_doing() {
    let stopped = |command: &str, timeout: u64, cap: usize| {
        run_command(
            command,
            Path::new("."),
            &env(),
            Duration::from_secs(timeout),
            cap,
            &StopSignal::default(),
        )
        .unwrap()
    };
    let timed_out = stopped("sleep 30 & sleep 30", 1, CAP);
    assert!(
        timed_out.timed_out && !timed_out.strays_stopped,
        "{timed_out:?}"
    );
    let overflowed = stopped("sleep 30 & yes", 30, 4096);
    assert!(
        overflowed.output_overflow && !overflowed.strays_stopped,
        "{overflowed:?}"
    );
}

/// A Windows job holds the step's own process while it runs.
#[test]
fn a_job_holds_others_when_more_run_in_it_than_the_step() {
    assert!(!run::others_in_job(1, true));
    assert!(run::others_in_job(2, true));
    assert!(run::others_in_job(1, false));
    assert!(!run::others_in_job(0, false));
}
