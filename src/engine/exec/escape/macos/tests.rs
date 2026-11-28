//! Tests of finding escaped processes on macOS: the token a process was
//! started with, what is recorded below a command, and what a pass stops.

use std::collections::{BTreeMap, BTreeSet};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::super::Running;
use super::*;

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
        let below: Vec<libc::pid_t> = tree.keys().copied().filter(|pid| *pid != group).collect();
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
