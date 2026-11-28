//! When a receipt is reused and when its step runs again.

use crate::support::{step_flow, Project, PROVEN};

#[test]
fn unchanged_inputs_reuse_the_receipt_and_reproduce_the_root() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    let first = project.flot(&["run", "--json"]);
    first.ok();
    let second = project.flot(&["run", "--json"]);
    second.ok();

    let first_root = first.json()["manifest"]["root_digest"].clone();
    let second_root = second.json()["manifest"]["root_digest"].clone();
    assert_eq!(
        first_root, second_root,
        "a reused run must fold to the same root"
    );
    assert_eq!(second.json()["steps"][0]["cached"], serde_json::json!(true));
    assert_eq!(first.json()["steps"][0]["cached"], serde_json::json!(false));
}

#[test]
fn changing_a_declared_input_invalidates_the_cache() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    project.write("seed.txt", "two");
    let second = project.flot(&["run", "--json"]);
    second.ok();
    assert_eq!(
        second.json()["steps"][0]["cached"],
        serde_json::json!(false)
    );
}

#[test]
fn a_failed_step_is_retried_instead_of_replayed_from_the_cache() {
    let project = Project::new(
        r#"
[flow]
name = "flaky"

[[step]]
id = "flaky"
run = "if [ -f attempted ]; then echo recovered; else touch attempted; exit 1; fi"
"#,
    );
    assert_eq!(project.flot(&["run"]).code, 1);
    let second = project.flot(&["run", "--json"]);
    second.ok();
    assert_eq!(
        second.json()["steps"][0]["cached"],
        serde_json::json!(false)
    );
    assert_eq!(second.json()["steps"][0]["status"], "ok");
}

#[test]
fn a_refuted_claim_is_re_proved_on_the_next_run() {
    let project = Project::new(
        r#"
[flow]
name = "refuted-then-fixed"

[[step]]
id = "check"
run = "true"
claims = ["the fix is in place"]
verify = "test -f fixed"
"#,
    );
    assert_eq!(project.flot(&["run"]).code, 1);
    project.write("fixed", "");
    project.flot(&["run"]).ok().has("PASSED");
}

#[test]
fn force_re_runs_a_step_that_would_have_been_reused() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    let forced = project.flot(&["run", "--force", "--json"]);
    forced.ok();
    assert_eq!(
        forced.json()["steps"][0]["cached"],
        serde_json::json!(false)
    );
}

#[test]
fn re_executing_the_same_work_leaves_earlier_runs_verifiable() {
    let project = Project::new(
        "[flow]\nname = \"again\"\n\n[[step]]\nid = \"ok\"\nrun = \"echo hi\"\n\n[[step]]\nid = \"bad\"\nrun = \"exit 1\"\nallow_failure = false\n",
    );
    // Same identity each time, different timing: the receipt file is shared,
    // so each run must keep its own observation.
    assert_eq!(project.flot(&["run"]).code, 1);
    let first = std::fs::read_to_string(project.path().join(".flot/latest")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(project.flot(&["run", "--force"]).code, 1);
    let output = project.flot(&["verify", first.trim()]);
    output.has("integrity: intact");
    let bundle = project.path().join("first.json");
    project
        .flot(&["bundle", first.trim(), "-o", bundle.to_str().unwrap()])
        .ok();
    let target =
        Project::new("[flow]\nname = \"again\"\n\n[[step]]\nid = \"ok\"\nrun = \"echo hi\"\n");
    target.flot(&["import", bundle.to_str().unwrap()]).ok();
}

#[test]
fn adding_a_claim_to_a_cached_step_runs_its_proof() {
    let project = Project::new(&step_flow("run = \"true\"\nverify = \"false\""));
    project.flot(&["run"]).ok();
    let flow = std::fs::read_to_string(project.path().join("flot.toml")).unwrap();
    project.write("flot.toml", &format!("{flow}claims = [\"it works\"]\n"));
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.has("ran      s").has("claims 0v/0u/1r");
}

#[cfg(unix)]
#[test]
fn a_restored_output_keeps_its_executable_bit() {
    let project = Project::new(&step_flow(
        "run = \"printf '#!/bin/sh\\\\necho hi\\\\n' > tool.sh; chmod +x tool.sh\"\noutputs = [\"tool.sh\"]\nclaims = [\"runs\"]\nverify = \"./tool.sh\"",
    ));
    project.flot(&["run"]).ok().has("claims 1v/0u/0r");
    std::fs::remove_file(project.path().join("tool.sh")).unwrap();
    project.flot(&["run"]).ok().has("cached   s");
    assert!(std::process::Command::new(project.path().join("tool.sh"))
        .status()
        .unwrap()
        .success());
    // Left as the step left it, the output answers the receipt; with its
    // bit taken away, it does not.
    project
        .flot(&["run"])
        .ok()
        .has("cached   s")
        .lacks("changed since its receipt");
    use std::os::unix::fs::PermissionsExt;
    let tool = project.path().join("tool.sh");
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
    project
        .flot(&["run"])
        .has("output tool.sh changed since its receipt");
}

#[test]
fn why_a_receipt_was_passed_over_is_not_sealed_into_the_next_one() {
    let project = Project::new(
        "[flow]\nname = \"n\"\n\n[[step]]\nid = \"make\"\nrun = \"echo generated > out.txt\"\noutputs = [\"out.txt\"]\n\n[[step]]\nid = \"use\"\nneeds = [\"make\"]\nrun = \"cat out.txt\"\n",
    );
    let first = project.flot(&["run", "--json"]).json();
    project.write("out.txt", "edit\n");
    let second = project.flot(&["run", "--json"]);
    second.has("output out.txt changed since its receipt");
    let second = second.json();
    // Identical work, identical receipt: the dependent is reused.
    assert_eq!(
        first["steps"][0]["receipt_id"],
        second["steps"][0]["receipt_id"]
    );
    assert_eq!(second["steps"][1]["cached"], true);
}

#[test]
fn a_failed_attempt_does_not_evict_an_earlier_success() {
    let project = Project::new(&step_flow("run = \"echo work\""));
    project.flot(&["run"]).ok();
    assert_eq!(project.flot(&["run", "--force", "--max-cost", "0"]).code, 1);
    project.flot(&["run"]).ok().has("cached   s");
}

#[test]
fn a_tolerated_failure_is_never_reused() {
    let project = Project::new(
        r#"
[flow]
name = "lint"

[[step]]
id = "lint"
run = "test -e ready"
allow_failure = true
"#,
    );
    project
        .flot(&["run"])
        .ok()
        .has("exited 1 but the step is declared allow_failure");
    project.write("ready", "");
    project
        .flot(&["run"])
        .ok()
        .has("ran      lint")
        .lacks("exited 1");
    project.flot(&["run"]).ok().has("cached   lint");
}
