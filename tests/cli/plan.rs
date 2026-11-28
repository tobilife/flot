//! Planning a run and running part of a flow.

use crate::support::{run_id, Project, TWO_STEPS};

#[test]
fn plan_names_steps_that_claim_without_proof_and_runs_nothing() {
    let project = Project::new(
        r#"
[flow]
name = "planned"

[[step]]
id = "risky"
run = "touch plan-side-effect.txt"
claims = ["nothing broke"]
"#,
    );
    project.flot(&["plan"]).ok().has("NO PROOF").has("risky");
    assert!(!project.path().join("plan-side-effect.txt").exists());
}

#[test]
fn only_runs_the_requested_step_and_its_dependencies() {
    let project = Project::new(
        r#"
[flow]
name = "subset"

[[step]]
id = "first"
run = "echo first"

[[step]]
id = "second"
run = "echo second"
needs = ["first"]

[[step]]
id = "unrelated"
run = "touch unrelated-ran.txt"
"#,
    );
    project.flot(&["run", "--only", "second"]).partial();
    assert!(!project.path().join("unrelated-ran.txt").exists());
    let show = project.flot(&["show", "--json"]);
    show.partial();
    let steps = show.json()["steps"].as_array().unwrap().len();
    assert_eq!(steps, 2);
    show.has("partial run");
}

#[test]
fn plan_sees_the_files_reuse_will_restore() {
    let project = Project::new(
        "[flow]\nname = \"p\"\n\n[[step]]\nid = \"gen\"\nrun = \"echo data > gen.txt\"\noutputs = [\"gen.txt\"]\n\n[[step]]\nid = \"use\"\nneeds = [\"gen\"]\nrun = \"cat gen.txt\"\ninputs = [\"gen.txt\"]\n",
    );
    project.flot(&["run"]).ok();
    std::fs::remove_file(project.path().join("gen.txt")).unwrap();
    let plan = project.flot(&["plan", "--json"]).json();
    assert_eq!(plan["steps"][1]["cached"], true, "{plan}");
    project.flot(&["run"]).ok().has("cached   use");

    // A receipt that cannot be checked is a miss in plan, as in run.
    std::fs::remove_file(project.path().join("gen.txt")).unwrap();
    std::fs::create_dir(project.path().join("gen.txt")).unwrap();
    project
        .flot(&["plan"])
        .ok()
        .has("cached receipt not reused");
}

#[test]
fn plan_says_when_a_step_declares_no_inputs() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["plan"]).ok().has("declares no inputs");
}

const SELECTED: &str = r#"
[flow]
name = "selected"

[[step]]
id = "lint"
run = "true"
claims = ["lint is clean"]
verify = "true"

[[step]]
id = "test"
run = "false"
claims = ["the tests pass"]
verify = "true"
"#;

/// The newest run's manifest file, made writable.
fn latest_run_file(project: &Project) -> std::path::PathBuf {
    let path = project
        .path()
        .join(".flot/runs")
        .join(format!("{}.json", run_id(project)));
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

#[test]
fn a_partial_run_is_not_a_verdict_on_the_flow() {
    let project = Project::new(SELECTED);
    let run = project.flot(&["run", "--only", "lint"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    run.has("\nPARTIAL ")
        .lacks("PASSED")
        .has("partial run: 1 of 2 steps selected");
    let json = project.flot(&["--json", "run", "--only", "lint"]);
    assert_eq!(json.code, 2, "{}", json.stdout);
    let json = json.json();
    assert_eq!(
        json["manifest"]["partial"],
        serde_json::json!({ "selected": 1, "of": 2 })
    );
    assert_eq!(json["manifest"]["verdict"], "passed");
    assert_eq!(json["result"], "partial");

    let verified = project.flot(&["verify"]);
    assert_eq!(verified.code, 2, "{}", verified.stdout);
    verified
        .has("integrity: intact")
        .has("verdict partial (1 of 2 steps selected, and they passed)\n");
    let audit = project.flot(&["--json", "verify"]);
    assert_eq!(audit.code, 2);
    let audit = audit.json();
    assert_eq!(audit["result"], "partial");
    assert_eq!(audit["partial"]["of"], 2);
    let markdown = project.flot(&["verify", "--markdown"]);
    assert_eq!(markdown.code, 2);
    markdown.has("### flot: partial, 1 of 2 steps selected");
    let show = project.flot(&["show"]);
    assert_eq!(show.code, 2, "{}", show.stdout);
    show.has("verdict partial (1 of 2 steps selected, and they passed)  claims 1v/0u/0r");

    // A selected step that fails is a failure, partial or not.
    let failed = project.flot(&["run", "--only", "test"]);
    assert_eq!(failed.code, 1, "{}", failed.stdout);
    failed.has("\nFAILED ").lacks("PARTIAL");
    let verified = project.flot(&["verify"]);
    assert_eq!(verified.code, 1, "{}", verified.stdout);
    verified.has("verdict failed (1 of 2 steps selected)\n");

    // The selection is part of what a signature covers.
    project.flot(&["key", "init"]).ok();
    let signed = project.flot(&["run", "--only", "lint", "--sign"]);
    assert_eq!(signed.code, 2, "{}", signed.stdout);
    let path = latest_run_file(&project);
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    manifest.as_object_mut().unwrap().remove("partial");
    std::fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    let dropped = project.flot(&["verify"]);
    assert_eq!(dropped.code, 1, "{}", dropped.stdout);
    dropped.has("integrity: BROKEN").has("does not hold");

    // A selection that does not match the steps listed is refused unsigned.
    let unsigned = Project::new(SELECTED);
    unsigned.flot(&["run", "--only", "lint"]);
    let path = latest_run_file(&unsigned);
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    manifest["partial"] = serde_json::json!({ "selected": 2, "of": 2 });
    std::fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    let forged = unsigned.flot(&["verify"]);
    assert_eq!(forged.code, 1, "{}", forged.stdout);
    forged.has("partial run of 2 of 2 steps");

    // Selecting a step whose dependencies are the whole flow is a whole run,
    // recorded as before.
    let whole = Project::new(
        &SELECTED
            .replace("run = \"false\"", "run = \"true\"")
            .replace("id = \"test\"", "id = \"test\"\nneeds = [\"lint\"]"),
    );
    whole
        .flot(&["run", "--only", "test"])
        .ok()
        .has("\nPASSED ")
        .lacks("partial");
    let text = std::fs::read_to_string(latest_run_file(&whole)).unwrap();
    assert!(!text.contains("partial"), "{text}");
    whole.flot(&["verify"]).ok().has("verdict passed\n");
}
