//! What the macOS watcher keeps to: how often it looks, what wakes it,
//! and what it knows of a process.

use std::sync::Condvar;
use std::time::Duration;

/// How often the macOS watcher records the processes below each running
/// command. A process that dropped its environment and left its parent
/// within one interval is never seen; a shorter one costs more CPU for
/// as long as any command runs.
pub(super) const POLL: Duration = Duration::from_millis(20);

/// Wakes the macOS watcher when a command starts.
pub(in super::super) static STARTED: Condvar = Condvar::new();

pub(in super::super) struct Process {
    pub(super) pid: libc::pid_t,
    pub(super) ppid: libc::pid_t,
    pub(super) pgid: libc::pid_t,
    pub(super) zombie: bool,
    /// When it started, in seconds and microseconds.
    pub(in super::super) start: (u64, u64),
}

/// launchd, which macOS makes the parent of every process whose parent
/// exits.
pub(super) const LAUNCHD: libc::pid_t = 1;
