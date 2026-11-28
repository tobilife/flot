//! What a run concludes from its steps and their claims.

use crate::support::{step_flow, Project, PROVEN};

#[test]
fn a_proven_claim_passes_with_exit_zero() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project
        .flot(&["run"])
        .ok()
        .has("PASSED")
        .has("claims 1v/0u/0r");
}

#[test]
fn a_claim_without_a_verify_command_can_never_pass() {
    let project = Project::new(
        r#"
[flow]
name = "unproven"

[[step]]
id = "assert"
run = "true"
claims = ["the migration is safe"]
"#,
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 2, "stdout:\n{}", output.stdout);
    output
        .has("UNVERIFIED")
        .has("claims are unverified")
        .lacks("PASSED");
}

#[test]
fn a_refuted_claim_fails_the_run() {
    let project = Project::new(
        r#"
[flow]
name = "refuted"

[[step]]
id = "lie"
run = "echo nothing > out.txt"
claims = ["out.txt mentions flot"]
verify = "grep -q flot out.txt"
"#,
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.has("FAILED").has("claims 0v/0u/1r");
}

#[test]
fn a_failed_step_cancels_its_dependents_before_they_run() {
    let project = Project::new(
        r#"
[flow]
name = "cancel"

[[step]]
id = "broken"
run = "exit 7"

[[step]]
id = "downstream"
run = "touch should-not-exist.txt"
needs = ["broken"]
"#,
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1);
    output
        .has("cancelled")
        .has("dependency broken (failed) did not hold");
    assert!(
        !project.path().join("should-not-exist.txt").exists(),
        "a cancelled step must not cause side effects"
    );
}

#[test]
fn work_built_on_a_refuted_claim_is_not_run() {
    let project = Project::new(
        r#"
[flow]
name = "refuted-upstream"

[[step]]
id = "implement"
run = "echo draft > artifact.txt"
claims = ["artifact.txt is final"]
verify = "grep -q final artifact.txt"

[[step]]
id = "review"
run = "touch review-ran.txt"
needs = ["implement"]
"#,
    );
    let output = project.flot(&["run", "--json"]);
    assert_eq!(output.code, 1);
    assert_eq!(output.json()["steps"][1]["status"], "cancelled");
    assert!(!project.path().join("review-ran.txt").exists());
    assert!(output.json()["manifest"]["limitations"]
        .to_string()
        .contains("dependency implement (claims refuted) did not hold"));
}

#[test]
fn an_unproven_dependency_does_not_block_its_dependents() {
    let project = Project::new(
        r#"
[flow]
name = "unproven-upstream"

[[step]]
id = "implement"
run = "true"
claims = ["no proof was declared for this"]

[[step]]
id = "review"
run = "touch review-ran.txt"
needs = ["implement"]
"#,
    );
    assert_eq!(project.flot(&["run"]).code, 2);
    assert!(project.path().join("review-ran.txt").exists());
}

#[test]
fn a_failing_line_in_a_multi_line_step_fails_the_step() {
    let project = Project::new(
        "[flow]\nname = \"m\"\n\n[[step]]\nid = \"agent\"\nrun = \"\"\"\nfalse\necho recorded anyway\n\"\"\"\n",
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.has("failed").lacks("recorded anyway");
}

#[test]
fn step_output_cannot_drive_the_terminal() {
    let project = Project::new(
        "[flow]\nname = \"esc\"\n\n[[step]]\nid = \"s\"\nrun = \"printf 'a\\\\033[2Jb\\\\n'\"\n",
    );
    let output = project.flot(&["run"]);
    output.ok().has("a\\u{1b}[2Jb");
    assert!(!output.stdout.contains('\u{1b}'));
}

#[test]
fn a_step_that_cannot_run_is_recorded_and_the_run_still_closes() {
    let project = Project::new(
        r#"
[flow]
name = "unrunnable"

[[step]]
id = "first"
run = "echo hi"

[[step]]
id = "after"
needs = ["first"]
run = "echo after"
"#,
    );
    // Make it impossible to give an executed step its metrics path.
    std::fs::create_dir_all(project.path().join(".flot")).unwrap();
    std::fs::write(project.path().join(".flot/tmp"), "not a directory").unwrap();
    let output = project.flot(&["run"]);
    assert_eq!(
        output.code, 1,
        "stdout:\n{}\nstderr:\n{}",
        output.stdout, output.stderr
    );
    output
        .has("ran      first                failed")
        .has("first: could not run:")
        .has("skipped  after")
        .has("FAILED");
    let latest = std::fs::read_to_string(project.path().join(".flot/latest")).unwrap();
    assert!(
        output.stdout.contains(latest.trim()),
        "the run was recorded"
    );
}

#[test]
fn allow_failure_keeps_the_run_going_but_proves_nothing() {
    let project = Project::new(&step_flow(
        "run = \"exit 7\"\nallow_failure = true\nclaims = [\"done\"]\nverify = \"true\"",
    ));
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 2, "stdout:\n{}", output.stdout);
    output
        .has("exited 7 but the step is declared allow_failure")
        .has("claims 0v/1u/0r");
}
