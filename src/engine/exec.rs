//! Running one command under an admission limit, with its complete output
//! captured.
//!
//! Output is read on dedicated threads so a chatty step cannot deadlock on a
//! full pipe buffer, and a step that outlives its budget is stopped together
//! with the process group it leads rather than being left behind.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod escape;
mod interrupt;
#[cfg(windows)]
mod job;
mod output;
mod run;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(any(target_os = "linux", windows))]
use anyhow::Context;
use anyhow::Result;

use crate::core::flow::ResolvedBudget;

pub use interrupt::{install_interrupt_handlers, interrupted, StopSignal};
pub use run::{run_command, run_confined};

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
mod tests;
