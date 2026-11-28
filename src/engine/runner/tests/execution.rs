//! Running a step's command and judging what it left: a command that cannot
//! start, confined or not, a stray that `allow_failure` tolerates, and
//! outputs the store cannot take.

use std::collections::BTreeMap;

use super::support::Workspace;
use crate::core::receipt::{ClaimStatus, StepStatus};
use crate::engine::runner::RunOptions;

const START: &str = r#"
[flow]
name = "start"

[[step]]
id = "start"
run = "true"
claims = ["it started"]
verify = "true"
"#;

/// `START`, with `sandbox` added to its step and an environment no process
/// can be started with: a variable holding a NUL byte.
fn unstartable(sandbox: &str) -> Workspace {
    let mut workspace = Workspace::new(&format!("{START}{sandbox}"));
    workspace.flow.steps[0]
        .env
        .insert("BROKEN".into(), "a\0b".into());
    workspace
}

/// A step that asked for no sandbox and cannot start fails as one that
/// could not run: there is no sandbox to blame.
#[test]
fn a_step_that_cannot_start_fails_as_one_that_could_not_run() {
    let workspace = unstartable("");
    let report = workspace.run(&RunOptions::default());
    let identity = workspace.receipt(&report, "start").identity;
    assert_eq!(identity.status, StepStatus::Failed);
    assert_eq!(identity.exit_code, None);
    assert_eq!(identity.claims[0].status, ClaimStatus::Unverified);
    assert_eq!(
        identity.claims[0].reason.as_deref(),
        Some("step could not run")
    );
    let why = identity.limitations.last().unwrap();
    assert!(
        why.starts_with("could not run: step `start` could not start: "),
        "{why}"
    );
}

/// A confined step's sandbox is applied as it starts, so a step that cannot
/// start confined does not run at all, and says so.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_confined_step_that_cannot_start_could_not_run_in_its_sandbox() {
    let workspace = unstartable("sandbox = { network = false }\n");
    let report = workspace.run(&RunOptions::default());
    let identity = workspace.receipt(&report, "start").identity;
    assert_eq!(identity.status, StepStatus::Failed);
    assert_eq!(
        identity.claims[0].reason.as_deref(),
        Some("step could not run in its sandbox")
    );
    let why = identity.limitations.last().unwrap();
    assert!(
        why.starts_with("could not run: its sandbox cannot be enforced here: "),
        "{why}"
    );
}

/// Straying is a failure `allow_failure` lets pass, even after an exit 0:
/// the step is `ok`, and its claims are not proven.
#[test]
fn a_step_that_strays_under_allow_failure_proves_nothing() {
    let workspace = Workspace::new(
        r#"
[flow]
name = "stray"

[[step]]
id = "tidy"
run = '''printf 'stray\n' > stray.txt'''
may_change = ["tidy.log"]
allow_failure = true
claims = ["it tidied"]
verify = "true"
"#,
    );
    let report = workspace.run(&RunOptions::default());
    let identity = workspace.receipt(&report, "tidy").identity;
    assert_eq!(identity.status, StepStatus::Ok);
    assert_eq!(identity.exit_code, Some(0));
    assert!(
        identity.limitations.contains(
            &"changed files outside may_change (stray.txt), but the step is declared allow_failure"
                .to_string()
        ),
        "{:?}",
        identity.limitations
    );
    assert_eq!(identity.claims[0].status, ClaimStatus::Unverified);
    assert_eq!(
        identity.claims[0].reason.as_deref(),
        Some("step failed under allow_failure")
    );
    assert_eq!(identity.claims[0].verify_exit, None);
}

/// Outputs the store cannot take are not recorded, so the step did not
/// produce what the flow says it does.
#[test]
fn a_success_whose_outputs_cannot_be_recorded_fails() {
    let workspace = Workspace::new(
        r#"
[flow]
name = "keep"

[[step]]
id = "make"
run = '''printf 'made\n' > out.txt'''
inputs = ["src.txt"]
outputs = ["out.txt"]
"#,
    );
    workspace.write("src.txt", "fn main() {}\n");
    // Where the store would keep what the step makes is a file.
    let object = crate::core::digest::bytes(b"made\n");
    let objects = workspace.store.root().join("objects");
    std::fs::create_dir_all(&objects).unwrap();
    std::fs::write(objects.join(&object["sha256:".len()..][..2]), "").unwrap();

    let runner = workspace.runner();
    let step = workspace.flow.step("make").unwrap();
    let keyed = runner.keyed(step, runner.facts(step), Vec::new());
    let receipt = runner
        .execute(keyed, &BTreeMap::new(), &RunOptions::default(), &mut None)
        .unwrap();
    let identity = receipt.identity;
    assert_eq!(identity.exit_code, Some(0));
    assert_eq!(identity.status, StepStatus::Failed);
    assert!(identity.outputs.is_empty());
    assert_eq!(identity.limitations.len(), 2, "{:?}", identity.limitations);
    assert!(
        identity.limitations[0].starts_with("outputs could not be recorded: "),
        "{:?}",
        identity.limitations
    );
    assert_eq!(
        identity.limitations[1],
        "produced no file for declared output out.txt"
    );
}
