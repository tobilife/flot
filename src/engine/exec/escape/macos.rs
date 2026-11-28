//! Escaped processes on macOS, which has no subreaper: a watcher thread
//! records the processes below each running command, and libproc and
//! sysctl tell of each process.

mod watcher;

use super::{state, State, TOKEN_ENV};

pub(super) use watcher::{Process, STARTED};
use watcher::{LAUNCHD, POLL};

/// Start the watcher: while any command runs, it records the processes
/// below each every `POLL`, and while none runs it waits. Should no
/// thread start, only the token finds what leaves a group.
pub(super) fn watch_orphans() {
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

/// What the kernel tells of `pid`. It tells only of a process of flot's
/// own user, unless flot runs as root.
pub(super) fn inspect(pid: libc::pid_t) -> Option<Process> {
    // SAFETY: proc_bsdinfo holds only integers and arrays of them, for
    // which zero is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the kernel writes at most `size` bytes into `info`.
    let written =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
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
fn children(parent: libc::pid_t) -> Vec<libc::pid_t> {
    // SAFETY: libproc writes at most `room` bytes into `buffer`.
    pids(|buffer, room| unsafe { libc::proc_listchildpids(parent, buffer, room) })
}

/// Every process the kernel tells of.
fn processes() -> Vec<Process> {
    // SAFETY: libproc writes at most `room` bytes into `buffer`.
    pids(|buffer, room| unsafe { libc::proc_listallpids(buffer, room) })
        .into_iter()
        .filter_map(inspect)
        .collect()
}

/// The largest block of arguments and environment a process can have.
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
pub(super) fn pass(
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

#[cfg(test)]
mod tests;
