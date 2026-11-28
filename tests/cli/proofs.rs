//! What a proof may read, change and be judged by.

use crate::support::{step_flow, Project};

const GUARDED: &str = r#"
[flow]
name = "guarded"

[[step]]
id = "build"
run = "echo built > artifact.txt"
claims = ["the acceptance check passes"]
verify = "sh check.sh"
verify_inputs = ["check.sh"]
"#;

#[test]
fn tightening_the_proof_re_proves_instead_of_reusing_an_old_verdict() {
    let project = Project::new(GUARDED);
    project.write("check.sh", "grep -q built artifact.txt\n");
    project.flot(&["run"]).ok().has("PASSED");

    // The work is unchanged, but the proof now demands something it lacks.
    project.write("check.sh", "grep -q shipped artifact.txt\n");
    let output = project.flot(&["run", "--json"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    assert_eq!(
        output.json()["steps"][0]["cached"],
        serde_json::json!(false)
    );
    assert_eq!(output.json()["manifest"]["claims"]["refuted"], 1);
}

#[test]
fn a_step_that_rewrites_its_own_proof_cannot_verify() {
    let project = Project::new(&GUARDED.replace(
        "run = \"echo built > artifact.txt\"",
        "run = \"echo built > artifact.txt; echo 'exit 0' > check.sh\"",
    ));
    project.write("check.sh", "exit 1\n");
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 2, "stdout:\n{}", output.stdout);
    output
        .has("UNVERIFIED")
        .has("the step changed its own proof (check.sh)");
}

#[test]
fn verify_inputs_without_a_verify_command_is_refused() {
    let project = Project::new(&GUARDED.replace("verify = \"sh check.sh\"\n", ""));
    let output = project.flot(&["plan"]);
    assert_eq!(output.code, 3);
    output.has("verify_inputs without a verify command");
}

#[test]
fn a_proof_that_changes_what_it_judges_verifies_nothing() {
    let project = Project::new(&step_flow(
        "run = \"echo draft > out.txt\"\noutputs = [\"out.txt\"]\nclaims = [\"final\"]\nverify = \"echo final > out.txt; grep -qx final out.txt\"",
    ));
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 2, "stdout:\n{}", output.stdout);
    output.has("files the proof judged changed while it ran (out.txt)");
}

// Windows has no signals: a killed proof reports whatever exit code its
// killer chose, and is judged by it.
#[cfg(unix)]
#[test]
fn a_proof_killed_by_a_signal_is_unverified_not_refuted() {
    let project = Project::new(&step_flow(
        "run = \"true\"\nclaims = [\"x\"]\nverify = \"kill -9 $$\"",
    ));
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 2, "stdout:\n{}", output.stdout);
    output.has("killed by a signal").has("claims 0v/1u/0r");
}
