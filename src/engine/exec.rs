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

use crate::core::receipt::OMITTED;

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
    run_confined(command, cwd, env, timeout, max_output_bytes, stop, None)
}

/// [`run_command`], confined by `sandbox` when there is one.
pub fn run_confined(
    command: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    max_output_bytes: usize,
    stop: &StopSignal,
    sandbox: Option<&crate::engine::sandbox::Prepared>,
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

/// Processes that leave a command's process group, found and stopped on
/// Linux and macOS.
///
/// A process that calls `setsid` (or `setpgid`) is out of reach of the group
/// kill, and once its parent exits it would be adopted by init and keep
/// running, free to rewrite what a proof judged. Each command carries a token
/// in its environment, which a process inherits whatever group it moves to,
/// so an escapee is charged to the command that started it.
///
/// On Linux flot makes itself a child subreaper, so every such orphan below
/// it becomes its child, and after each command looks for adopted processes
/// that belong to no running command: those escaped, and are stopped. One
/// that dropped its environment is charged to every command running when it
/// was found.
///
/// macOS has no subreaper. While a command runs, a watcher thread records
/// every `POLL` each process below its leader, so one that left the group
/// is still known once its parent is gone. After the command, each recorded
/// process that launchd adopted, outside every running command's group,
/// escaped, and is stopped and charged to that command, whether or not it
/// kept its environment. So is such an orphan whose environment still
/// carries one of flot's tokens, which finds a daemon that forked away
/// between two looks. A process that dropped its environment and left its
/// parent within one interval is not found.
///
/// A process that moves into flot's own process group is not told apart
/// from the git processes flot runs itself, and is not found.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod escape {
    use std::collections::{BTreeMap, BTreeSet};
    use std::process::{Child, Command};
    use std::sync::atomic::{AtomicU64, Ordering};
    #[cfg(target_os = "macos")]
    use std::sync::Condvar;
    use std::sync::{Mutex, MutexGuard, Once};
    use std::time::{Duration, Instant};

    /// Names the command a process descends from.
    pub const TOKEN_ENV: &str = "FLOT_COMMAND_ID";

    /// How long a sweep waits for stopped processes to be gone.
    const SETTLE: Duration = Duration::from_secs(2);

    /// How often the macOS watcher records the processes below each running
    /// command. A process that dropped its environment and left its parent
    /// within one interval is never seen; a shorter one costs more CPU for
    /// as long as any command runs.
    #[cfg(target_os = "macos")]
    const POLL: Duration = Duration::from_millis(20);

    struct Running {
        /// The process group the command leads.
        group: libc::pid_t,
        token: String,
        /// Its leader has exited and its own sweep has begun.
        ended: bool,
        /// Pid of each process charged to it, and whether it was traced.
        escaped: BTreeMap<u32, bool>,
        /// The leader and every live process recorded below it, by pid, each
        /// with the time it started: a pid that is reused names a process
        /// that started later.
        #[cfg(target_os = "macos")]
        tree: BTreeMap<libc::pid_t, (u64, u64)>,
    }

    struct State {
        running: BTreeMap<u64, Running>,
        /// Groups of escaped processes already stopped, until they are empty.
        stopped: BTreeSet<libc::pid_t>,
    }

    static STATE: Mutex<State> = Mutex::new(State {
        running: BTreeMap::new(),
        stopped: BTreeSet::new(),
    });
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    static WATCH_ORPHANS: Once = Once::new();
    /// Wakes the macOS watcher when a command starts.
    #[cfg(target_os = "macos")]
    static STARTED: Condvar = Condvar::new();

    fn state() -> MutexGuard<'static, State> {
        STATE.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn token(id: u64) -> String {
        format!("{}-{id}", std::process::id())
    }

    /// One command, from before it is spawned until it is swept.
    pub struct Watch {
        id: u64,
    }

    impl Watch {
        /// Mark `command` with a token of its own, and from now on have every
        /// process orphaned below a command found.
        pub fn new(command: &mut Command) -> Watch {
            WATCH_ORPHANS.call_once(watch_orphans);
            let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            command.env(TOKEN_ENV, token(id));
            Watch { id }
        }

        /// Spawn `command`, recorded as running before any sweep can see it.
        pub fn spawn(&self, command: &mut Command) -> std::io::Result<Child> {
            let mut state = state();
            let child = command.spawn()?;
            let group = child.id() as libc::pid_t;
            state.running.insert(
                self.id,
                Running {
                    group,
                    token: token(self.id),
                    ended: false,
                    escaped: BTreeMap::new(),
                    // Should its start not be readable, nothing is recorded
                    // below it, and only its token finds what leaves.
                    #[cfg(target_os = "macos")]
                    tree: inspect(group)
                        .map(|leader| (group, leader.start))
                        .into_iter()
                        .collect(),
                },
            );
            #[cfg(target_os = "macos")]
            STARTED.notify_one();
            Ok(child)
        }

        /// Once the command's leader is reaped and its group stopped: finish
        /// what is left of the group, stop every process found to have left
        /// a group no running command leads, and return the pids of those
        /// charged to this command.
        pub fn sweep(self) -> Vec<super::Escaped> {
            let me = std::process::id() as libc::pid_t;
            // SAFETY: getpgrp cannot fail.
            let own_group = unsafe { libc::getpgrp() };
            let deadline = Instant::now() + SETTLE;
            loop {
                let busy = {
                    let mut state = state();
                    let Some(running) = state.running.get_mut(&self.id) else {
                        break;
                    };
                    running.ended = true;
                    let group = running.group;
                    pass(&mut state, me, own_group, group)
                };
                if !busy || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            state()
                .running
                .get(&self.id)
                .map(|running| {
                    running
                        .escaped
                        .iter()
                        .map(|(&pid, &traced)| super::Escaped { pid, traced })
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            state().running.remove(&self.id);
        }
    }

    /// Have flot adopt every process orphaned below it.
    #[cfg(target_os = "linux")]
    fn watch_orphans() {
        // SAFETY: sets a flag of this process; no memory is passed. Should
        // it fail, orphans go to init as before and none is found.
        unsafe {
            libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
        }
    }

    #[cfg(target_os = "linux")]
    struct Process {
        pid: libc::pid_t,
        ppid: libc::pid_t,
        pgid: libc::pid_t,
        zombie: bool,
    }

    /// Every process, read from /proc: after the parenthesized command name
    /// of /proc/<pid>/stat come its state, parent and process group.
    #[cfg(target_os = "linux")]
    fn processes() -> Vec<Process> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let pid = entry.file_name().to_str()?.parse::<libc::pid_t>().ok()?;
                let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
                let after = &stat[stat.rfind(')')? + 1..];
                let mut fields = after.split_whitespace();
                let zombie = fields.next()? == "Z";
                let ppid = fields.next()?.parse().ok()?;
                let pgid = fields.next()?.parse().ok()?;
                Some(Process {
                    pid,
                    ppid,
                    pgid,
                    zombie,
                })
            })
            .collect()
    }

    /// The token in the environment `pid` was started with, if it kept one.
    #[cfg(target_os = "linux")]
    fn token_of(pid: libc::pid_t) -> Option<String> {
        let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
        let prefix = format!("{TOKEN_ENV}=");
        environ.split(|byte| *byte == 0).find_map(|entry| {
            entry
                .strip_prefix(prefix.as_bytes())
                .map(|value| String::from_utf8_lossy(value).into_owned())
        })
    }

    /// Reap `process` if it has exited, else stop it. Only for a child of
    /// flot not yet reaped, whose pid therefore names it and nothing else.
    #[cfg(target_os = "linux")]
    fn finish(process: &Process) {
        // SAFETY: plain system calls on a pid flot's own unreaped child holds.
        unsafe {
            if process.zombie {
                libc::waitpid(process.pid, std::ptr::null_mut(), libc::WNOHANG);
            } else {
                libc::kill(process.pid, libc::SIGKILL);
            }
        }
    }

    /// One look at flot's children. Returns whether anything is still to be
    /// waited for.
    #[cfg(target_os = "linux")]
    fn pass(
        state: &mut State,
        me: libc::pid_t,
        own_group: libc::pid_t,
        group: libc::pid_t,
    ) -> bool {
        let processes = processes();
        state
            .stopped
            .retain(|stopped| processes.iter().any(|process| process.pgid == *stopped));
        let mut busy = false;
        for process in &processes {
            let adopted = process.ppid == me;
            if process.pgid == group || state.stopped.contains(&process.pgid) {
                busy = true;
                if adopted {
                    finish(process);
                }
                continue;
            }
            // flot's own children (git) share its group; a running
            // command's leader and its group are that command's to stop.
            if !adopted
                || process.pgid == own_group
                || state
                    .running
                    .values()
                    .any(|running| running.group == process.pgid)
            {
                continue;
            }
            if process.zombie {
                // It ended on its own before anything could stop it.
                finish(process);
                continue;
            }
            let pid = process.pid as u32;
            let token = token_of(process.pid);
            let owner = token.as_deref().and_then(|token| {
                state
                    .running
                    .values_mut()
                    .find(|running| running.token == token)
            });
            match owner {
                // Its command is still running, and will sweep it itself.
                Some(owner) if !owner.ended => continue,
                Some(owner) => {
                    owner.escaped.insert(pid, true);
                }
                None => {
                    for running in state.running.values_mut() {
                        running.escaped.entry(pid).or_insert(false);
                    }
                }
            }
            busy = true;
            finish(process);
            if process.pgid == process.pid {
                // A group it leads is its own: stop all of it. The pid is
                // still held by the unreaped process, so it names that group.
                // SAFETY: a plain system call.
                unsafe {
                    libc::killpg(process.pid, libc::SIGKILL);
                }
                state.stopped.insert(process.pid);
            }
        }
        busy
    }

    /// Start the watcher: while any command runs, it records the processes
    /// below each every `POLL`, and while none runs it waits. Should no
    /// thread start, only the token finds what leaves a group.
    #[cfg(target_os = "macos")]
    fn watch_orphans() {
        let _ = std::thread::Builder::new()
            .name("flot-escape".into())
            .spawn(|| loop {
                {
                    let mut state = state();
                    while state.running.is_empty() {
                        state = STARTED
                            .wait(state)
                            .unwrap_or_else(|poison| poison.into_inner());
                    }
                    record(&mut state);
                }
                std::thread::sleep(POLL);
            });
    }

    #[cfg(target_os = "macos")]
    struct Process {
        pid: libc::pid_t,
        ppid: libc::pid_t,
        pgid: libc::pid_t,
        zombie: bool,
        /// When it started, in seconds and microseconds.
        start: (u64, u64),
    }

    /// launchd, which macOS makes the parent of every process whose parent
    /// exits.
    #[cfg(target_os = "macos")]
    const LAUNCHD: libc::pid_t = 1;

    /// What the kernel tells of `pid`. It tells only of a process of flot's
    /// own user, unless flot runs as root.
    #[cfg(target_os = "macos")]
    fn inspect(pid: libc::pid_t) -> Option<Process> {
        // SAFETY: proc_bsdinfo holds only integers and arrays of them, for
        // which zero is a valid value.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: the kernel writes at most `size` bytes into `info`.
        let written = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size)
        };
        (written == size).then_some(Process {
            pid,
            ppid: info.pbi_ppid as libc::pid_t,
            pgid: info.pbi_pgid as libc::pid_t,
            zombie: info.pbi_status == libc::SZOMB,
            start: (info.pbi_start_tvsec, info.pbi_start_tvusec),
        })
    }

    /// The pids a libproc listing writes, given a buffer and its size in
    /// bytes, and returning how many it wrote. A full buffer may have cut
    /// the list short, so the listing is retried with twice the room.
    #[cfg(target_os = "macos")]
    fn pids(list: impl Fn(*mut libc::c_void, libc::c_int) -> libc::c_int) -> Vec<libc::pid_t> {
        let mut pids: Vec<libc::pid_t> = vec![0; 256];
        loop {
            let Ok(room) = libc::c_int::try_from(std::mem::size_of_val(pids.as_slice())) else {
                return Vec::new();
            };
            let Ok(written) = usize::try_from(list(pids.as_mut_ptr().cast(), room)) else {
                return Vec::new();
            };
            if written < pids.len() {
                pids.truncate(written);
                return pids;
            }
            pids.resize(pids.len() * 2, 0);
        }
    }

    /// The pids whose parent is `parent`.
    #[cfg(target_os = "macos")]
    fn children(parent: libc::pid_t) -> Vec<libc::pid_t> {
        // SAFETY: libproc writes at most `room` bytes into `buffer`.
        pids(|buffer, room| unsafe { libc::proc_listchildpids(parent, buffer, room) })
    }

    /// Every process the kernel tells of.
    #[cfg(target_os = "macos")]
    fn processes() -> Vec<Process> {
        // SAFETY: libproc writes at most `room` bytes into `buffer`.
        pids(|buffer, room| unsafe { libc::proc_listallpids(buffer, room) })
            .into_iter()
            .filter_map(inspect)
            .collect()
    }

    /// The largest block of arguments and environment a process can have.
    #[cfg(target_os = "macos")]
    fn argument_space() -> Option<usize> {
        let mut name = [libc::CTL_KERN, libc::KERN_ARGMAX];
        let mut space: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        // SAFETY: the kernel writes at most `size` bytes into `space`.
        let rc = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                2,
                (&raw mut space).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            usize::try_from(space).ok()
        } else {
            None
        }
    }

    /// The token in the environment `pid` was started with, if it kept one.
    /// KERN_PROCARGS2 gives the process's argument count, then the path it
    /// was started from, padded with NULs, then each argument and each
    /// environment entry, NUL terminated; the kernel gives it only for a
    /// process of flot's own user. `area` is sized once to hold the largest
    /// such block, and reused from one process to the next.
    #[cfg(target_os = "macos")]
    fn token_of(pid: libc::pid_t, area: &mut Vec<u8>) -> Option<String> {
        if area.is_empty() {
            area.resize(argument_space()?, 0);
        }
        let mut name = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size = area.len();
        // SAFETY: the kernel writes at most `size` bytes into `area`, and
        // sets `size` to how many it wrote.
        let rc = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                3,
                area.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        let (count, strings) = area.get(..size)?.split_first_chunk::<4>()?;
        let count = usize::try_from(i32::from_ne_bytes(*count)).ok()?;
        let mut strings = strings.split(|byte| *byte == 0);
        strings.next()?;
        let prefix = format!("{TOKEN_ENV}=");
        strings
            .skip_while(|string| string.is_empty())
            .skip(count)
            .take_while(|string| !string.is_empty())
            .find_map(|entry| {
                entry
                    .strip_prefix(prefix.as_bytes())
                    .map(|value| String::from_utf8_lossy(value).into_owned())
            })
    }

    /// Add to each running command every process whose parent is recorded
    /// for it, and the processes below those, after forgetting the recorded
    /// ones that are gone: their pids may name other processes by now.
    #[cfg(target_os = "macos")]
    fn record(state: &mut State) {
        for running in state.running.values_mut() {
            running.tree.retain(|&pid, start| {
                inspect(pid).is_some_and(|now| now.start == *start && !now.zombie)
            });
            let mut parents: Vec<libc::pid_t> = running.tree.keys().copied().collect();
            while let Some(parent) = parents.pop() {
                for child in children(parent) {
                    if running.tree.contains_key(&child) {
                        continue;
                    }
                    if let Some(found) = inspect(child).filter(|found| !found.zombie) {
                        running.tree.insert(child, found.start);
                        parents.push(child);
                    }
                }
            }
        }
    }

    /// Stop `process`, with the group it leads if it leads one, unless its
    /// pid has come to name another process: a process flot did not start
    /// can exit, and be reaped by its own parent, at any moment.
    #[cfg(target_os = "macos")]
    fn stop(process: &Process) {
        if !inspect(process.pid).is_some_and(|now| now.start == process.start) {
            return;
        }
        // SAFETY: plain system calls. The pid was just seen to name the
        // process, which as a group's leader also holds that group's id.
        unsafe {
            if process.pgid == process.pid {
                libc::killpg(process.pid, libc::SIGKILL);
            } else {
                libc::kill(process.pid, libc::SIGKILL);
            }
        }
    }

    /// One look at every process, after recording what is new below the
    /// running commands: stop what is left of the command's group, and each
    /// orphan outside the groups running commands lead that is recorded
    /// below one of them or carries a token of flot, whose pid is `me`.
    /// Returns whether anything is still to be waited for.
    #[cfg(target_os = "macos")]
    fn pass(
        state: &mut State,
        me: libc::pid_t,
        own_group: libc::pid_t,
        group: libc::pid_t,
    ) -> bool {
        record(state);
        let processes = processes();
        state.stopped.retain(|stopped| {
            processes
                .iter()
                .any(|process| process.pgid == *stopped && !process.zombie)
        });
        // A process that started before flot did cannot carry its token.
        let since = processes
            .iter()
            .find(|process| process.pid == me)
            .map_or((0, 0), |flot| flot.start);
        let ours = format!("{me}-");
        let mut area = Vec::new();
        let mut busy = false;
        for process in &processes {
            // It has exited: nothing is left to stop, and its parent reaps
            // it.
            if process.zombie {
                continue;
            }
            if process.pgid == group || state.stopped.contains(&process.pgid) {
                busy = true;
                stop(process);
                continue;
            }
            // As on Linux, only an orphan escaped: one whose parent still
            // runs goes with the group of the orphan above it, or is an
            // orphan itself once that one is stopped. flot's own children
            // (git) share its group; a running command's leader and its
            // group are that command's to stop.
            if process.ppid != LAUNCHD
                || process.pgid == own_group
                || state
                    .running
                    .values()
                    .any(|running| running.group == process.pgid)
            {
                continue;
            }
            let token = if process.start >= since {
                token_of(process.pid, &mut area).filter(|token| token.starts_with(&ours))
            } else {
                None
            };
            let recorded = state
                .running
                .iter()
                .find(|(_, running)| running.tree.get(&process.pid) == Some(&process.start))
                .map(|(&id, _)| id);
            if token.is_none() && recorded.is_none() {
                continue;
            }
            // Its token names its command; without one, the command it was
            // recorded below does.
            let owner = token
                .as_deref()
                .and_then(|token| {
                    state
                        .running
                        .iter()
                        .find(|(_, running)| running.token == token)
                })
                .map(|(&id, _)| id)
                .or(recorded);
            let pid = process.pid as u32;
            match owner.and_then(|id| state.running.get_mut(&id)) {
                // Its command is still running, and will sweep it itself.
                Some(owner) if !owner.ended => continue,
                Some(owner) => {
                    owner.escaped.insert(pid, true);
                }
                None => {
                    for running in state.running.values_mut() {
                        running.escaped.entry(pid).or_insert(false);
                    }
                }
            }
            busy = true;
            stop(process);
            if process.pgid == process.pid {
                state.stopped.insert(process.pid);
            }
        }
        busy
    }

    #[cfg(all(test, target_os = "macos"))]
    mod tests {
        use super::*;
        use std::process::Stdio;

        fn start(command: &mut Command) -> Child {
            command.stdin(Stdio::null()).spawn().unwrap()
        }

        /// One command, led by `group`, as its sweep sees it: its leader
        /// has exited, and `tree` was recorded below it.
        fn ended(group: libc::pid_t, token: &str, tree: Vec<(libc::pid_t, (u64, u64))>) -> State {
            let mut state = State {
                running: BTreeMap::new(),
                stopped: BTreeSet::new(),
            };
            state.running.insert(
                0,
                Running {
                    group,
                    token: token.to_owned(),
                    ended: true,
                    escaped: BTreeMap::new(),
                    tree: tree.into_iter().collect(),
                },
            );
            state
        }

        fn alive(process: &Process) -> bool {
            inspect(process.pid).is_some_and(|now| now.start == process.start && !now.zombie)
        }

        /// Whether `process` has exited, waiting up to a few seconds for it.
        fn gone(process: &Process) -> bool {
            let deadline = Instant::now() + Duration::from_secs(5);
            while alive(process) {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            true
        }

        /// Start a process with `token` in its environment that leaves its
        /// group, and wait until it has and its parent is gone. Returns the
        /// pid of that parent, and the process.
        fn daemon(token: &str) -> (libc::pid_t, Process) {
            let parent = start(
                Command::new("/bin/sh")
                    .args([
                        "-c",
                        "/usr/bin/perl -MPOSIX -e 'setsid(); exec @ARGV' /bin/sleep 5 >/dev/null & echo $!",
                    ])
                    .env(TOKEN_ENV, token)
                    .stdout(Stdio::piped()),
            );
            let leader = parent.id() as libc::pid_t;
            let output = parent.wait_with_output().unwrap();
            let pid: libc::pid_t = String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(process) = inspect(pid).filter(|process| process.pgid == pid) {
                    return (leader, process);
                }
                assert!(Instant::now() < deadline, "{pid} never left its group");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        #[test]
        fn a_token_is_read_from_the_environment_a_process_started_with() {
            let mut marked = start(Command::new("/bin/sleep").arg("5").env(TOKEN_ENV, "1-2"));
            let mut cleared = start(Command::new("/bin/sleep").arg("5").env_clear());
            // An argument that looks like one is not one.
            let mut argued = start(
                Command::new("/bin/sh")
                    .args(["-c", "sleep 5; :", "sh", &format!("{TOKEN_ENV}=1-3")])
                    .env_clear(),
            );
            let mut area = Vec::new();
            let mut token = |child: &Child| token_of(child.id() as libc::pid_t, &mut area);
            assert_eq!(token(&marked).as_deref(), Some("1-2"));
            assert_eq!(token(&cleared), None);
            assert_eq!(token(&argued), None);
            for child in [&mut marked, &mut cleared, &mut argued] {
                let _ = child.kill();
                let _ = child.wait();
            }
        }

        #[test]
        fn a_process_stays_recorded_once_its_parent_is_gone() {
            let mut leader = start(Command::new("/bin/sh").args(["-c", "sleep 5 & sleep 0.3; :"]));
            let group = leader.id() as libc::pid_t;
            let mut state = ended(group, "", vec![(group, inspect(group).unwrap().start)]);
            let deadline = Instant::now() + Duration::from_secs(5);
            let below = loop {
                record(&mut state);
                let tree = &state.running[&0].tree;
                let below: Vec<libc::pid_t> =
                    tree.keys().copied().filter(|pid| *pid != group).collect();
                if below.len() == 2 || Instant::now() >= deadline {
                    break below;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(below.len(), 2, "both sleeps are recorded: {below:?}");
            leader.wait().unwrap();
            std::thread::sleep(Duration::from_millis(100));
            record(&mut state);
            let left: Vec<libc::pid_t> = state.running[&0].tree.keys().copied().collect();
            assert_eq!(left.len(), 1, "only `sleep 5` is left: {left:?}");
            assert!(below.contains(&left[0]), "{left:?} {below:?}");
            for pid in left {
                stop(&inspect(pid).unwrap());
            }
        }

        #[test]
        fn a_pid_that_names_a_later_process_is_neither_kept_nor_stopped() {
            let mut child = start(Command::new("/bin/sleep").arg("5"));
            let now = inspect(child.id() as libc::pid_t).unwrap();
            // What was recorded of an earlier process with the same pid.
            let earlier = Process {
                start: (now.start.0 - 1, now.start.1),
                ..now
            };
            stop(&earlier);
            std::thread::sleep(Duration::from_millis(100));
            assert!(
                child.try_wait().unwrap().is_none(),
                "a later process was stopped"
            );
            let mut state = ended(now.pid, "", vec![(now.pid, earlier.start)]);
            record(&mut state);
            assert!(
                state.running[&0].tree.is_empty(),
                "a later process was kept"
            );
            stop(&now);
            assert!(!child.wait().unwrap().success());
        }

        #[test]
        fn an_orphan_is_charged_to_the_command_its_token_names_and_to_no_other_flot() {
            // SAFETY: getpgrp cannot fail.
            let own_group = unsafe { libc::getpgrp() };
            // The tokens of a flot with pid 1, which no real one has: the
            // flots running these tests, this one included, pass them by.
            let flot = 1;
            let (leader, ours) = daemon("1-0");
            let mut state = ended(leader, "1-0", Vec::new());
            pass(&mut state, flot, own_group, leader);
            let charged: Vec<(u32, bool)> = state.running[&0]
                .escaped
                .iter()
                .map(|(&pid, &traced)| (pid, traced))
                .collect();
            assert_eq!(charged, [(ours.pid as u32, true)]);
            assert!(gone(&ours), "the orphan outlived the sweep");

            let (leader, theirs) = daemon("2-0");
            let mut state = ended(leader, "1-0", Vec::new());
            pass(&mut state, flot, own_group, leader);
            assert!(state.running[&0].escaped.is_empty());
            std::thread::sleep(Duration::from_millis(100));
            assert!(alive(&theirs), "another flot's orphan was stopped");
            stop(&theirs);
            assert!(gone(&theirs));
        }
    }
}

/// A Windows job object holding one step and every process it starts.
#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
    };

    /// The step starts suspended, so it is in its job before it can start
    /// anything, and in its own process group, so a console Ctrl-C reaches
    /// flot and not the step.
    pub const SPAWN_FLAGS: u32 = CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP;

    pub struct Job(HANDLE);

    impl Drop for Job {
        // Closing the last handle kills whatever is still in the job, so a
        // step cannot outlive the flot that ran it.
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    fn last_error() -> std::io::Error {
        std::io::Error::last_os_error()
    }

    impl Job {
        /// Put a suspended child in a new job, then let it run.
        pub fn adopt(child: &std::process::Child) -> std::io::Result<Job> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(last_error());
            }
            let job = Job(handle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if set == 0 {
                return Err(last_error());
            }
            if unsafe { AssignProcessToJobObject(handle, child.as_raw_handle() as HANDLE) } == 0 {
                return Err(last_error());
            }
            resume(child.id())?;
            Ok(job)
        }

        /// Processes in the job that have not exited.
        pub fn active(&self) -> u32 {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            let ok = unsafe {
                QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    (&raw mut info).cast(),
                    std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                0
            } else {
                info.ActiveProcesses
            }
        }

        pub fn stop(&self) {
            unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    /// Resume the threads of a process created suspended: its one main thread.
    fn resume(pid: u32) -> std::io::Result<()> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = false;
        let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if !thread.is_null() {
                    resumed |= unsafe { ResumeThread(thread) } != u32::MAX;
                    unsafe { CloseHandle(thread) };
                }
            }
            more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot) };
        if resumed {
            Ok(())
        } else {
            Err(last_error())
        }
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

/// Bounded inline view of a step's output: its first lines and its last,
/// where builds and test runners report what failed, joined by
/// [`OMITTED`]. Returns the preview and whether anything was left out; the
/// complete stream is always kept in the store.
pub fn stream_preview(data: &[u8], max_bytes: usize) -> (String, bool) {
    if data.len() <= max_bytes {
        return (String::from_utf8_lossy(data).into_owned(), false);
    }
    let Some(room) = max_bytes
        .checked_sub(OMITTED.len())
        .filter(|room| *room >= 64)
    else {
        return inline_preview(data, max_bytes);
    };
    let tail_room = room * 3 / 4;
    let mut head = room - tail_room;
    while head > 0 && (data[head] & 0b1100_0000) == 0b1000_0000 {
        head -= 1;
    }
    // End the beginning at a line break; the marker starts a new line.
    if let Some(newline) = data[..head].iter().rposition(|byte| *byte == b'\n') {
        head = newline;
    }
    let mut tail = data.len() - tail_room;
    while tail < data.len() && (data[tail] & 0b1100_0000) == 0b1000_0000 {
        tail += 1;
    }
    // Start the end on a whole line.
    if let Some(newline) = data[tail..].iter().position(|byte| *byte == b'\n') {
        if tail + newline + 1 < data.len() {
            tail += newline + 1;
        }
    }
    let preview = format!(
        "{}{OMITTED}{}",
        String::from_utf8_lossy(&data[..head]),
        String::from_utf8_lossy(&data[tail..])
    );
    (preview, true)
}

/// The beginning of a stored object, at most `max_bytes`, and whether it was
/// cut there.
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

    #[test]
    fn a_long_stream_keeps_its_first_and_last_lines() {
        let data: String = (1..=3000).map(|n| format!("line {n}\n")).collect();
        let (text, truncated) = stream_preview(data.as_bytes(), 2048);
        assert!(truncated);
        assert!(text.len() <= 2048, "{}", text.len());
        assert!(text.starts_with("line 1\nline 2\n"), "{text}");
        assert!(text.ends_with("line 2999\nline 3000\n"), "{text}");
        let (head, tail) = text.split_once(OMITTED).unwrap();
        assert!(head.lines().all(|line| line.starts_with("line ")), "{head}");
        assert!(tail.lines().all(|line| line.starts_with("line ")), "{tail}");
        let observed = crate::core::receipt::Observed {
            stdout_inline: text.clone(),
            stdout_truncated: true,
            stdout_bytes: data.len(),
            ..Default::default()
        };
        assert!(observed.describes(data.as_bytes(), b""));
        // A preview whose end was never printed is a forgery.
        let forged = crate::core::receipt::Observed {
            stdout_inline: text.replace("line 3000", "line 9999"),
            ..observed
        };
        assert!(!forged.describes(data.as_bytes(), b""));
    }

    #[test]
    fn a_stream_preview_never_splits_a_character_or_exceeds_its_budget() {
        let data = "한글 ".repeat(2000);
        let (text, truncated) = stream_preview(data.as_bytes(), 500);
        assert!(truncated);
        assert!(text.len() <= 500, "{}", text.len());
        assert!(!text.contains('\u{FFFD}'), "{text}");
        let (head, tail) = text.split_once(OMITTED).unwrap();
        assert!(data.starts_with(head) && data.ends_with(tail));
        // Too small a budget for both ends keeps the beginning.
        let (text, truncated) = stream_preview(data.as_bytes(), 40);
        assert!(truncated);
        assert!(!text.contains(OMITTED) && data.starts_with(&text));
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
