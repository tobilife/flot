//! Processes that leave a command's process group, found and stopped on
//! Linux and macOS.
//!
//! A process that calls `setsid` (or `setpgid`) is out of reach of the group
//! kill, and once its parent exits it would be adopted by init and keep
//! running, free to rewrite what a proof judged. Each command carries a token
//! in its environment, which a process inherits whatever group it moves to,
//! so an escapee is charged to the command that started it.
//!
//! On Linux flot makes itself a child subreaper, so every such orphan below
//! it becomes its child, and after each command looks for adopted processes
//! that belong to no running command: those escaped, and are stopped. One
//! that dropped its environment is charged to every command running when it
//! was found.
//!
//! macOS has no subreaper. While a command runs, a watcher thread records
//! every `POLL` each process below its leader, so one that left the group
//! is still known once its parent is gone. After the command, each recorded
//! process that launchd adopted, outside every running command's group,
//! escaped, and is stopped and charged to that command, whether or not it
//! kept its environment. So is such an orphan whose environment still
//! carries one of flot's tokens, which finds a daemon that forked away
//! between two looks. A process that dropped its environment and left its
//! parent within one interval is not found.
//!
//! A process that moves into flot's own process group is not told apart
//! from the git processes flot runs itself, and is not found.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use std::collections::{BTreeMap, BTreeSet};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use self::linux::{pass, watch_orphans};
#[cfg(target_os = "macos")]
use self::macos::{inspect, pass, watch_orphans, STARTED};

/// Names the command a process descends from.
pub const TOKEN_ENV: &str = "FLOT_COMMAND_ID";

/// How long a sweep waits for stopped processes to be gone.
const SETTLE: Duration = Duration::from_secs(2);

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
    /// Orphans whose environment read empty, and when it first did.
    #[cfg(target_os = "linux")]
    unread: BTreeMap<libc::pid_t, Instant>,
}

static STATE: Mutex<State> = Mutex::new(State {
    running: BTreeMap::new(),
    stopped: BTreeSet::new(),
    #[cfg(target_os = "linux")]
    unread: BTreeMap::new(),
});
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static WATCH_ORPHANS: Once = Once::new();

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
