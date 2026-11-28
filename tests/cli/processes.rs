//! Timeouts, signals and the processes a step leaves behind.

use crate::support::Project;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::support::read;

#[cfg(unix)]
use crate::support::flot_bin;

#[cfg(unix)]
use std::process::Command;

#[test]
fn a_step_over_budget_is_a_timeout_not_a_success() {
    let project = Project::new(
        r#"
[flow]
name = "slow"

[[step]]
id = "hang"
run = "sleep 30"
budget = { timeout_secs = 1 }
claims = ["the work finished"]
verify = "true"
"#,
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output
        .has("timeout")
        .has("claims are unverified because the step did not succeed");
}

#[test]
fn a_step_that_backgrounds_work_does_not_hold_the_run_open() {
    let project = Project::new(
        "[flow]\nname = \"bg\"\nbudget = { timeout_secs = 60 }\n\n[[step]]\nid = \"leak\"\nrun = \"echo started; sleep 30 &\"\n",
    );
    let started = std::time::Instant::now();
    project
        .flot(&["run"])
        .ok()
        .has("left processes running when it exited; they were stopped");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[cfg(unix)]
#[test]
fn stopping_flot_stops_the_step_and_still_records_the_run() {
    let project = Project::new(
        "[flow]\nname = \"sig\"\n\n[[step]]\nid = \"s\"\nrun = \"sleep 30; touch survived\"\n\n[[step]]\nid = \"t\"\nneeds = [\"s\"]\nrun = \"echo after\"\n",
    );
    let child = Command::new(flot_bin())
        .arg("run")
        .current_dir(project.path())
        .env("FLOT_SIGNING_KEY", project.key_path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let started = std::time::Instant::now();
    let output = child.wait_with_output().unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("stopped because flot was asked to stop"),
        "{stdout}"
    );
    assert!(
        stdout.contains("t: not run: flot was asked to stop"),
        "{stdout}"
    );
    project.flot(&["verify"]).has("integrity: intact");
}

/// `flow` with `setsid sh` as this system runs it: macOS ships no `setsid`
/// command, so perl's POSIX::setsid stands in, by absolute paths because
/// `env -i` clears PATH.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn with_setsid(flow: &str) -> String {
    if cfg!(target_os = "macos") {
        flow.replace(
            "setsid sh",
            "/usr/bin/perl -MPOSIX -e 'setsid(); exec @ARGV' /bin/sh",
        )
    } else {
        flow.to_owned()
    }
}

/// A step that waits for its sibling to start, then leaves behind a process
/// that left its process group.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const ESCAPING: &str = r#"
[flow]
name = "escaping"

[[step]]
id = "leaves"
run = """
while [ ! -e .stays ]; do sleep 0.01; done
env -i setsid sh -c 'touch .left; sleep 3' >/dev/null 2>&1 &
while [ ! -e .left ]; do sleep 0.01; done
rm .left
"""
claims = ["it ran"]
verify = "true"

[[step]]
id = "stays"
run = "touch .stays; sleep 1; rm .stays"
claims = ["it ran"]
verify = "true"
"#;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_process_that_leaves_its_process_group_is_stopped_and_proves_nothing() {
    let project = Project::new(&with_setsid(
        r#"
[flow]
name = "escape"

[[step]]
id = "work"
run = """
echo 1 > out.txt
setsid sh -c 'touch .left; sleep 1; echo 0 > out.txt' >/dev/null 2>&1 &
while [ ! -e .left ]; do sleep 0.01; done
rm .left
"""
claims = ["out.txt holds 1"]
verify = "grep -q 1 out.txt"
"#,
    ));
    let run = project.flot(&["run"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    run.has("claims 0v/1u/0r")
        .has("  - work: a process it started left its process group and was stopped (pid ");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(
        read(&project, "out.txt"),
        "1\n",
        "the escapee outlived the run"
    );

    // A proof is held to the same: what it leaves behind voids its pass,
    // and a refutation still stands.
    for (last, code, claims) in [
        ("true", 2, "claims 0v/1u/0r"),
        ("false", 1, "claims 0v/0u/1r"),
    ] {
        let proof = Project::new(&with_setsid(&format!(
            r#"
[flow]
name = "escape"

[[step]]
id = "work"
run = "true"
claims = ["it ran"]
verify = """
setsid sh -c 'touch .left; sleep 3' >/dev/null 2>&1 &
while [ ! -e .left ]; do sleep 0.01; done
rm .left
{last}
"""
"#
        )));
        let run = proof.flot(&["run"]);
        assert_eq!(run.code, code, "{}", run.stdout);
        run.has(claims)
            .has("  - work: a process it started left its process group and was stopped (pid ");
    }

    // One that dropped its environment cannot be traced, so every step
    // running when it was found answers for it. On macOS flot recorded it
    // below the step that started it, and that step alone answers for it;
    // the step waits a moment before it ends, since flot looks for what is
    // below a step only every 20 ms and must see the escapee's parent once.
    let untraced_flow = if cfg!(target_os = "macos") {
        ESCAPING.replace("rm .left\n", "rm .left\nsleep 0.1\n")
    } else {
        ESCAPING.to_owned()
    };
    let pair = Project::new(&with_setsid(&untraced_flow));
    let untraced = pair.flot(&["run", "--jobs", "2"]);
    assert_eq!(untraced.code, 2, "{}", untraced.stdout);
    if cfg!(target_os = "macos") {
        untraced
            .has("  - leaves: a process it started left its process group and was stopped (pid ")
            .lacks("while this step ran");
        assert!(
            untraced
                .stdout
                .lines()
                .any(|line| line.starts_with("ran      stays") && line.contains("claims 1v/0u/0r")),
            "{}",
            untraced.stdout
        );
    } else {
        untraced.has(
            "  - leaves, stays: a process that left its process group was stopped while this step ran (pid ",
        );
    }
    // One that kept it is charged to the step that started it alone.
    pair.write("flot.toml", &with_setsid(&ESCAPING.replace("env -i ", "")));
    // On macOS `stays` passed above and would be reused, and `leaves` would
    // wait for it forever.
    let traced = pair.flot(if cfg!(target_os = "macos") {
        &["run", "--jobs", "2", "--force"]
    } else {
        &["run", "--jobs", "2"]
    });
    assert_eq!(traced.code, 2, "{}", traced.stdout);
    traced
        .has("  - leaves: a process it started left its process group and was stopped (pid ")
        .lacks("while this step ran");
    assert!(
        traced
            .stdout
            .lines()
            .any(|line| line.starts_with("ran      stays") && line.contains("claims 1v/0u/0r")),
        "{}",
        traced.stdout
    );
}
