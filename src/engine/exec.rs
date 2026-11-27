//! Running one command under an admission limit, with its complete output
//! captured.
//!
//! Output is read on dedicated threads so a chatty step cannot deadlock on a
//! full pipe buffer, and a step that outlives its budget is stopped together
//! with the process group it leads rather than being left behind.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod escape;
#[cfg(windows)]
mod job;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::core::flow::ResolvedBudget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    /// The signal that ended the command, when one did.
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    /// flot was asked to stop while the command ran, and stopped it.
    pub interrupted: bool,
    /// A stream passed its byte budget and the command was stopped there.
    pub output_overflow: bool,
    /// Processes the command started were still running when it exited, and
    /// were stopped. A step is one unit of work; nothing it starts outlives it.
    pub strays_stopped: bool,
    /// A process that left the step's process group still held its output
    /// open after the grace period, so the captured streams may be incomplete.
    pub output_detached: bool,
    /// Processes that left their process group and were still running when
    /// the command ended, stopped. Linux and macOS only, where flot can find
    /// them; a Windows job cannot be left, and elsewhere this is always
    /// empty.
    pub escaped: Vec<Escaped>,
    pub started_at_ms: u64,
    pub duration_ms: u64,
}

/// A process that left its command's process group, and was stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escaped {
    pub pid: u32,
    /// Traced to this command by the token it inherited or, on macOS, by
    /// having been recorded below the command's leader. Otherwise nothing
    /// tells which command started it, and it is charged to every command
    /// that was running when it was found.
    pub traced: bool,
}

/// How long output is still read once the step's own process has gone and its
/// process group was stopped. Only a process that left the group can hold the
/// pipes past that point, and waiting for it could mean waiting forever.
const DETACHED_GRACE: Duration = Duration::from_secs(2);

/// Which of a command's output streams a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Told each line a command prints as it is read: without its `\n` or
/// `\r\n`, and a last one that has neither once its stream ends. Only what
/// is kept for the record is told, so a stream past its budget stops where
/// its record is cut.
pub type Lines = Arc<dyn Fn(Stream, &[u8]) + Send + Sync>;

/// How long a command may run and how many bytes each of its streams may
/// print before it is stopped.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

impl Limits {
    /// The limits a step's budget sets.
    pub fn of(budget: ResolvedBudget) -> Self {
        Limits {
            timeout: Duration::from_secs(budget.timeout_secs),
            max_output_bytes: budget.max_output_bytes,
        }
    }
}

impl Outcome {
    pub fn succeeded(&self) -> bool {
        !self.timed_out && !self.interrupted && !self.output_overflow && self.exit_code == Some(0)
    }
}

/// Milliseconds as recorded in receipts and manifests. JSON tooling and
/// canonical digests stop at 64 bits, so that is the width used everywhere.
pub fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(millis)
        .unwrap_or_default()
}

/// Set once flot is asked to stop (SIGINT, SIGTERM, SIGHUP). Running commands
/// are stopped with their process groups, steps not yet started are cancelled,
/// and the run still records what happened.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Whether flot has been asked to stop.
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// One run's way to be withdrawn by whoever started it (an MCP host
/// cancelling a tool call). A withdrawn run stops exactly as an interrupted
/// one does, recording what happened, but flot stays up and other runs are
/// untouched.
#[derive(Debug, Clone, Default)]
pub struct StopSignal(Arc<AtomicBool>);

impl StopSignal {
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether the run must stop: it was withdrawn, or flot was interrupted.
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst) || interrupted()
    }
}

/// Route SIGINT, SIGTERM and SIGHUP to the interrupt flag. Every step leads
/// its own process group, so a terminal's Ctrl-C reaches only flot; without
/// this, flot would die and leave the step running, unrecorded. A second
/// signal exits at once, for a user who does not want to wait.
pub fn install_interrupt_handlers() {
    // Windows: Ctrl-C and closing the console. Steps start in their own
    // process group, which a console Ctrl-C does not reach.
    #[cfg(windows)]
    {
        unsafe extern "system" fn on_ctrl(_: u32) -> windows_sys::core::BOOL {
            if INTERRUPTED.swap(true, Ordering::SeqCst) {
                unsafe { windows_sys::Win32::System::Threading::ExitProcess(130) };
            }
            1
        }
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(on_ctrl), 1);
        }
    }
    #[cfg(unix)]
    {
        extern "C" fn on_signal(_: libc::c_int) {
            if INTERRUPTED.swap(true, Ordering::SeqCst) {
                // Only async-signal-safe calls here.
                unsafe { libc::_exit(130) };
            }
        }
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            unsafe {
                libc::signal(signal, handler);
            }
        }
    }
}

/// The POSIX shell steps run in. Flows are written for `sh` on every
/// platform, so one flow file means the same commands everywhere; Windows has
/// none of its own, and Git for Windows ships the one flot uses there.
#[cfg(not(windows))]
fn shell() -> Result<std::path::PathBuf> {
    Ok("sh".into())
}

#[cfg(windows)]
fn shell() -> Result<std::path::PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<std::path::PathBuf> = std::env::split_paths(&path).collect();
    if let Some(sh) = dirs
        .iter()
        .map(|dir| dir.join("sh.exe"))
        .find(|sh| sh.is_file())
    {
        return Ok(sh);
    }
    // Git for Windows puts only `cmd\git.exe` on PATH; its shell is beside it
    // in `bin`, which also sets up the POSIX tools a step expects.
    let beside_git = dirs
        .iter()
        .filter(|dir| dir.join("git.exe").is_file())
        .filter_map(|dir| dir.parent().map(|git| git.join("bin").join("sh.exe")));
    let installed = ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
        .flat_map(|base| {
            let base = std::path::PathBuf::from(base);
            [
                base.join("Git").join("bin").join("sh.exe"),
                base.join("Programs").join("Git").join("bin").join("sh.exe"),
            ]
        });
    beside_git
        .chain(installed)
        .find(|sh| sh.is_file())
        .context("flot runs steps with a POSIX `sh`, and none was found; install Git for Windows, which provides one, or put sh.exe on PATH")
}

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
        let others = step_job.active() > u32::from(leader_running);
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

/// Whether the command's process has exited, without reaping it. Reaping
/// frees its pid, and with it the id of the group it led, before the rest of
/// that group can be stopped.
fn has_exited(child: &mut std::process::Child) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error()).context("cannot wait for a step");
        }
        Ok(unsafe { info.si_pid() } != 0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(child.try_wait()?.is_some())
    }
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

/// A stream being read on its own thread. What has arrived so far stays
/// available even if the stream never closes.
struct Drain {
    buffer: Arc<Mutex<Vec<u8>>>,
    done: mpsc::Receiver<()>,
    /// Set once the command's output is collected: a line read after that
    /// is told to no one.
    collected: Arc<AtomicBool>,
}

impl Drain {
    /// Everything read by `deadline`, and whether the stream had closed.
    fn collect(self, deadline: Instant) -> (Vec<u8>, bool) {
        let complete = !matches!(
            self.done
                .recv_timeout(deadline.saturating_duration_since(Instant::now())),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        self.collected.store(true, Ordering::SeqCst);
        let data = std::mem::take(&mut *self.buffer.lock().unwrap_or_else(|e| e.into_inner()));
        (data, complete)
    }
}

/// A stream cut into lines for [`Lines`] as it is read.
struct Splitter {
    stream: Stream,
    lines: Lines,
    /// The line read so far, which has not ended yet.
    partial: Vec<u8>,
    collected: Arc<AtomicBool>,
}

impl Splitter {
    fn feed(&mut self, mut bytes: &[u8]) {
        while let Some(end) = bytes.iter().position(|&byte| byte == b'\n') {
            self.partial.extend_from_slice(&bytes[..end]);
            self.tell();
            bytes = &bytes[end + 1..];
        }
        self.partial.extend_from_slice(bytes);
    }

    /// The stream ended: a last line without a line ending is a line too.
    fn finish(&mut self) {
        if !self.partial.is_empty() {
            self.tell();
        }
    }

    fn tell(&mut self) {
        let line = std::mem::take(&mut self.partial);
        if !self.collected.load(Ordering::SeqCst) {
            (self.lines)(self.stream, line.strip_suffix(b"\r").unwrap_or(&line));
        }
    }
}

/// Read `source` on its own thread, keeping at most `cap` bytes. Reaching the
/// cap raises `overflow`, which stops the command; reading continues, and
/// discards, so the command is never blocked on a full pipe meanwhile. The
/// bytes kept are also told to `lines`, line by line, when it is given.
fn drain<R: Read + Send + 'static>(
    source: Option<R>,
    cap: usize,
    overflow: Arc<AtomicBool>,
    lines: Option<(Stream, Lines)>,
) -> Drain {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let (sender, done) = mpsc::channel();
    let collected = Arc::new(AtomicBool::new(false));
    let shared = Arc::clone(&buffer);
    let mut splitter = lines.map(|(stream, lines)| Splitter {
        stream,
        lines,
        partial: Vec::new(),
        collected: Arc::clone(&collected),
    });
    std::thread::spawn(move || {
        if let Some(mut source) = source {
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                match source.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        let taken = {
                            let mut kept = shared.lock().unwrap_or_else(|e| e.into_inner());
                            let room = cap.saturating_sub(kept.len());
                            kept.extend_from_slice(&chunk[..read.min(room)]);
                            if read > room {
                                overflow.store(true, Ordering::SeqCst);
                            }
                            read.min(room)
                        };
                        if let Some(splitter) = &mut splitter {
                            splitter.feed(&chunk[..taken]);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        if let Some(splitter) = &mut splitter {
            splitter.finish();
        }
        let _ = sender.send(());
    });
    Drain {
        buffer,
        done,
        collected,
    }
}

/// Stop every process in the group the step led. Returns whether anything
/// besides the leader's zombie was there to stop. Call it before the leader
/// is reaped: until then its pid pins the group id, so this cannot reach an
/// unrelated group.
#[cfg(not(windows))]
fn stop_group(child: &std::process::Child) -> bool {
    #[cfg(target_os = "linux")]
    {
        let group = child.id() as libc::pid_t;
        // A group holding only the exited leader accepts the signal too, so
        // look for live members before deciding anything was left behind.
        let others = live_members(group);
        unsafe {
            libc::killpg(group, libc::SIGKILL);
        }
        others
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) == 0
    }
    #[cfg(not(unix))]
    {
        let _ = child;
        false
    }
}

/// Whether any process other than the leader belongs to `group`, read from
/// /proc: field 5 of /proc/<pid>/stat is the process group id.
#[cfg(target_os = "linux")]
fn live_members(group: libc::pid_t) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<libc::pid_t>().ok()) else {
            return false;
        };
        if pid == group {
            return false;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            return false;
        };
        // The command name is parenthesized and may contain spaces.
        let Some(after) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
            return false;
        };
        let fields: Vec<&str> = after.split_whitespace().collect();
        // after ')': state ppid pgrp ...
        fields.get(2).and_then(|f| f.parse::<libc::pid_t>().ok()) == Some(group)
            && fields.first() != Some(&"Z")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 1 << 20;

    fn env() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn run(command: &str) -> Outcome {
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
        assert!(!outcome.output_detached && outcome.strays_stopped);
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

    // Windows has no signals: a killed process exits with whatever code its
    // killer chose.
    #[cfg(unix)]
    #[test]
    fn a_signal_death_is_reported_as_a_signal() {
        let outcome = run("kill -9 $$");
        assert_eq!(outcome.exit_code, None);
        assert_eq!(outcome.signal, Some(9));
    }

    #[test]
    fn large_output_does_not_deadlock() {
        let outcome = run("yes flot | head -c 300000");
        assert_eq!(outcome.stdout.len(), 300_000);
        assert!(outcome.succeeded());
    }
}
