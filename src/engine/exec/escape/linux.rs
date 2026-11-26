//! Escaped processes on Linux, where flot is the child subreaper of
//! everything below it and reads each process from /proc.

use std::time::{Duration, Instant};

use super::{State, TOKEN_ENV};

/// Have flot adopt every process orphaned below it.
pub(super) fn watch_orphans() {
    // SAFETY: sets a flag of this process; no memory is passed. Should
    // it fail, orphans go to init as before and none is found.
    unsafe {
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    }
}

struct Process {
    pid: libc::pid_t,
    ppid: libc::pid_t,
    pgid: libc::pid_t,
    zombie: bool,
}

/// Every process, read from /proc: after the parenthesized command name
/// of /proc/<pid>/stat come its state, parent and process group.
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

/// How long an orphan whose environment reads empty is looked at again
/// before it counts as having none. It reads empty while the process is
/// inside `execve`, between dropping the old program and setting up the
/// new one's environment; that takes far less than this.
const UNREAD: Duration = Duration::from_millis(50);

/// What the environment a process was started with says of its command.
#[derive(Debug, PartialEq, Eq)]
enum Mark {
    Token(String),
    /// It has an environment, without a token in it.
    Tokenless,
    /// Its environment read empty, or could not be read.
    Unread,
}

fn mark_of(pid: libc::pid_t) -> Mark {
    let environ = match std::fs::read(format!("/proc/{pid}/environ")) {
        Ok(environ) if !environ.is_empty() => environ,
        _ => return Mark::Unread,
    };
    let prefix = format!("{TOKEN_ENV}=");
    environ
        .split(|byte| *byte == 0)
        .find_map(|entry| {
            entry
                .strip_prefix(prefix.as_bytes())
                .map(|value| Mark::Token(String::from_utf8_lossy(value).into_owned()))
        })
        .unwrap_or(Mark::Tokenless)
}

/// Reap `process` if it has exited, else stop it. Only for a child of
/// flot not yet reaped, whose pid therefore names it and nothing else.
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
pub(super) fn pass(
    state: &mut State,
    me: libc::pid_t,
    own_group: libc::pid_t,
    group: libc::pid_t,
) -> bool {
    let processes = processes();
    state
        .stopped
        .retain(|stopped| processes.iter().any(|process| process.pgid == *stopped));
    state.unread.retain(|pid, _| {
        processes
            .iter()
            .any(|process| process.pid == *pid && process.ppid == me && !process.zombie)
    });
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
        let token = match mark_of(process.pid) {
            Mark::Token(token) => Some(token),
            Mark::Tokenless => None,
            Mark::Unread => {
                let since = *state.unread.entry(process.pid).or_insert_with(Instant::now);
                if since.elapsed() < UNREAD {
                    busy = true;
                    continue;
                }
                None
            }
        };
        state.unread.remove(&process.pid);
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::process::{Child, Command};

    use super::super::Running;
    use super::*;

    fn comm(pid: libc::pid_t) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default()
    }

    #[test]
    fn an_empty_environment_is_unread_not_tokenless() {
        let sleep = || {
            let mut command = Command::new("/bin/sleep");
            command.arg("5").env_clear();
            command
        };
        let mut marked = sleep().env(TOKEN_ENV, "1-2").spawn().unwrap();
        let mut other = sleep().env("A", "1").spawn().unwrap();
        let mut empty = sleep().spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mark = |child: &Child| loop {
            let mark = mark_of(child.id() as libc::pid_t);
            if mark != Mark::Unread || Instant::now() >= deadline {
                break mark;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(mark(&marked), Mark::Token("1-2".into()));
        assert_eq!(mark(&other), Mark::Tokenless);
        assert_eq!(mark_of(empty.id() as libc::pid_t), Mark::Unread);
        for child in [&mut marked, &mut other, &mut empty] {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[test]
    fn an_orphan_whose_environment_reads_empty_is_looked_at_again_first() {
        // A shell stands in for flot, so that no other test's process is
        // its child; what it starts drops its environment. Both stay in
        // this process's group, where the sweeps of other tests look past
        // them, and this pass is told of no group of flot's own.
        let mut parent = Command::new("/bin/sh")
            .args(["-c", "env -i /bin/sleep 5 & wait"])
            .spawn()
            .unwrap();
        let me = parent.id() as libc::pid_t;
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            let found = processes()
                .into_iter()
                .find(|process| process.ppid == me && comm(process.pid).trim() == "sleep");
            if let Some(process) = found {
                break process.pid;
            }
            assert!(Instant::now() < deadline, "sleep never started");
            std::thread::sleep(Duration::from_millis(5));
        };
        let own_group = 0;
        let mut state = State {
            running: BTreeMap::new(),
            stopped: BTreeSet::new(),
            unread: BTreeMap::new(),
        };
        state.running.insert(
            0,
            Running {
                group: 0,
                token: "1-0".into(),
                ended: true,
                escaped: BTreeMap::new(),
            },
        );
        assert!(pass(&mut state, me, own_group, 0), "it is waited for");
        assert!(state.running[&0].escaped.is_empty(), "charged at once");
        assert!(state.unread.contains_key(&pid));
        assert_eq!(comm(pid).trim(), "sleep", "stopped at once");
        // Still empty once the wait is over: it has no environment.
        state.unread.insert(pid, Instant::now() - UNREAD);
        pass(&mut state, me, own_group, 0);
        let charged: Vec<(u32, bool)> = state.running[&0]
            .escaped
            .iter()
            .map(|(&pid, &traced)| (pid, traced))
            .collect();
        assert_eq!(charged, [(pid as u32, false)]);
        assert!(!state.unread.contains_key(&pid));
        assert!(parent.wait().is_ok());
    }
}
