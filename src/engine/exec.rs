//! Running one command under an admission limit, with its complete output
//! captured.
//!
//! Output is read on dedicated threads so a chatty step cannot deadlock on a
//! full pipe buffer, and a step that outlives its budget is stopped together
//! with the process group it leads rather than being left behind.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

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
    pub started_at_ms: u64,
    pub duration_ms: u64,
}

/// How long output is still read once the step's own process has gone and its
/// process group was stopped. Only a process that left the group can hold the
/// pipes past that point, and waiting for it could mean waiting forever.
const DETACHED_GRACE: Duration = Duration::from_secs(2);

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
    let started_at_ms = now_ms();
    let started = Instant::now();

    let mut builder = Command::new("sh");
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
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Lead a new process group so a timeout can stop descendants too.
        builder.process_group(0);
    }

    let mut child = builder
        .spawn()
        .with_context(|| format!("cannot start `{command}`"))?;

    let overflow = Arc::new(AtomicBool::new(false));
    let stdout_reader = drain(child.stdout.take(), max_output_bytes, Arc::clone(&overflow));
    let stderr_reader = drain(child.stderr.take(), max_output_bytes, Arc::clone(&overflow));

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
    let stopped_others = stop_group(&child);
    let _ = child.kill();
    let status = child.wait().ok();
    let ended_on_its_own = !(timed_out || was_interrupted || overflowed);
    let strays_stopped = ended_on_its_own && stopped_others;
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
}

impl Drain {
    /// Everything read by `deadline`, and whether the stream had closed.
    fn collect(self, deadline: Instant) -> (Vec<u8>, bool) {
        let complete = !matches!(
            self.done
                .recv_timeout(deadline.saturating_duration_since(Instant::now())),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        let data = std::mem::take(&mut *self.buffer.lock().unwrap_or_else(|e| e.into_inner()));
        (data, complete)
    }
}

/// Read `source` on its own thread, keeping at most `cap` bytes. Reaching the
/// cap raises `overflow`, which stops the command; reading continues, and
/// discards, so the command is never blocked on a full pipe meanwhile.
fn drain<R: Read + Send + 'static>(
    source: Option<R>,
    cap: usize,
    overflow: Arc<AtomicBool>,
) -> Drain {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let (sender, done) = mpsc::channel();
    let shared = Arc::clone(&buffer);
    std::thread::spawn(move || {
        if let Some(mut source) = source {
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                match source.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        let mut kept = shared.lock().unwrap_or_else(|e| e.into_inner());
                        let room = cap.saturating_sub(kept.len());
                        kept.extend_from_slice(&chunk[..read.min(room)]);
                        if read > room {
                            overflow.store(true, Ordering::SeqCst);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        let _ = sender.send(());
    });
    Drain { buffer, done }
}

/// Stop every process in the group the step led. Returns whether anything
/// besides the leader's zombie was there to stop. Call it before the leader
/// is reaped: until then its pid pins the group id, so this cannot reach an
/// unrelated group.
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

/// Bounded inline view of a captured stream. Returns the preview text and
/// whether it was cut. The complete stream is always kept in the store.
pub fn inline_preview(data: &[u8], max_bytes: usize) -> (String, bool) {
    if data.len() <= max_bytes {
        return (String::from_utf8_lossy(data).into_owned(), false);
    }
    let mut cut = max_bytes;
    // Do not split a UTF-8 sequence; walk back to a boundary.
    while cut > 0 && (data[cut] & 0b1100_0000) == 0b1000_0000 {
        cut -= 1;
    }
    (String::from_utf8_lossy(&data[..cut]).into_owned(), true)
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
        assert!(outcome.output_detached);
        assert_eq!(outcome.stdout, b"early\n");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "{:?}",
            started.elapsed()
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

    #[test]
    fn preview_keeps_short_output_whole() {
        let (text, truncated) = inline_preview(b"short", 100);
        assert_eq!(text, "short");
        assert!(!truncated);
    }

    #[test]
    fn preview_cuts_long_output_and_says_so() {
        let (text, truncated) = inline_preview(b"0123456789", 4);
        assert_eq!(text, "0123");
        assert!(truncated);
    }

    #[test]
    fn preview_never_splits_a_multibyte_character() {
        let data = "한글".as_bytes();
        let (text, truncated) = inline_preview(data, 4);
        assert!(truncated);
        assert_eq!(text, "한");
    }
}
