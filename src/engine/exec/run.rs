//! Running one command through the shell within its limits: started apart
//! from flot, stopped with everything it started, and its output collected.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::escape;
#[cfg(windows)]
use super::job;
use super::output::drain;
#[cfg(not(windows))]
use super::stop_group;
use super::{has_exited, millis, now_ms, shell, Limits, Lines, Outcome, StopSignal, Stream};

/// How long output is still read once the step's own process has gone and its
/// process group was stopped. Only a process that left the group can hold the
/// pipes past that point, and waiting for it could mean waiting forever.
const DETACHED_GRACE: Duration = Duration::from_secs(2);

/// Execute `command` through `sh -e -c` from `cwd`.
///
/// `-e` makes a multi-line command fail at its first failing line. Without it
/// the exit status is only the last line's, so an agent call that failed
/// followed by a line that succeeded would be recorded as success.
///
/// The parent environment is inherited (a step needs `PATH` and the agent
/// credentials it was launched with); declared variables are applied on top.
/// A stream past `max_output_bytes` stops the command.
pub fn run_command(
    command: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    max_output_bytes: usize,
    stop: &StopSignal,
) -> Result<Outcome> {
    let limits = Limits {
        timeout,
        max_output_bytes,
    };
    run_confined(command, cwd, env, limits, stop, None, None)
}

/// [`run_command`], confined by `sandbox` when there is one, telling `lines`
/// what it prints as it prints it when they are given.
pub fn run_confined(
    command: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    Limits {
        timeout,
        max_output_bytes,
    }: Limits,
    stop: &StopSignal,
    sandbox: Option<&crate::engine::sandbox::Prepared>,
    lines: Option<&Lines>,
) -> Result<Outcome> {
    let started_at_ms = now_ms();
    let started = Instant::now();

    let shell = shell()?;
    let mut builder = match sandbox {
        Some(sandbox) => sandbox.command(&shell),
        None => Command::new(&shell),
    };
    builder
        .arg("-e")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        builder.env(key, value);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let watch = escape::Watch::new(&mut builder);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Lead a new process group so a timeout can stop descendants too.
        builder.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        builder.creation_flags(job::SPAWN_FLAGS);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let spawned = watch.spawn(&mut builder);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let spawned = builder.spawn();
    let mut child = spawned.with_context(|| format!("cannot start `{command}`"))?;
    // Windows has no process groups to signal; a job object holds the step
    // and everything it starts instead.
    #[cfg(windows)]
    let step_job = match job::Job::adopt(&child) {
        Ok(step_job) => step_job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("cannot hold the step in a job object");
        }
    };

    let overflow = Arc::new(AtomicBool::new(false));
    let told = |stream| lines.map(|lines| (stream, Arc::clone(lines)));
    let stdout_reader = drain(
        child.stdout.take(),
        max_output_bytes,
        Arc::clone(&overflow),
        told(Stream::Stdout),
    );
    let stderr_reader = drain(
        child.stderr.take(),
        max_output_bytes,
        Arc::clone(&overflow),
        told(Stream::Stderr),
    );

    let mut timed_out = false;
    let mut was_interrupted = false;
    let mut overflowed = false;
    loop {
        if has_exited(&mut child)? {
            break;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            break;
        }
        if stop.is_set() {
            was_interrupted = true;
            break;
        }
        if overflow.load(Ordering::SeqCst) {
            overflowed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // The leader has exited or is about to be stopped, and is not reaped yet,
    // so its pid still names this group and no other. Whatever is left in the
    // group is stopped: background processes the command left behind would
    // keep running and, holding the output pipes, keep this call from
    // returning at all.
    #[cfg(not(windows))]
    let stopped_others = stop_group(&child);
    #[cfg(windows)]
    let stopped_others = {
        let leader_running = matches!(child.try_wait(), Ok(None));
        let others = others_in_job(step_job.active(), leader_running);
        step_job.stop();
        others
    };
    let _ = child.kill();
    let status = child.wait().ok();
    let ended_on_its_own = !(timed_out || was_interrupted || overflowed);
    let strays_stopped = ended_on_its_own && stopped_others;
    // Before the output is collected: a process that left the group and
    // still holds the pipes closes them as it is stopped.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let escaped = watch.sweep();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let escaped = Vec::new();
    let deadline = Instant::now() + DETACHED_GRACE;
    let (stdout, stdout_complete) = stdout_reader.collect(deadline);
    let (stderr, stderr_complete) = stderr_reader.collect(deadline);

    Ok(Outcome {
        exit_code: status.and_then(|status| status.code()),
        signal: status.and_then(signal_of),
        stdout,
        stderr,
        timed_out,
        interrupted: was_interrupted,
        output_overflow: overflowed,
        strays_stopped,
        output_detached: !(stdout_complete && stderr_complete),
        escaped,
        started_at_ms,
        duration_ms: millis(started.elapsed()),
    })
}

/// Whether a job holding `active` processes holds any besides the step's
/// own, which is one of them while it runs.
#[cfg(any(windows, test))]
pub(super) fn others_in_job(active: u32, leader_running: bool) -> bool {
    active > u32::from(leader_running)
}

fn signal_of(status: std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}
