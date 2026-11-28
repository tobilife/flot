//! The files a step changes and the receipt that answers for them.

use crate::support::Project;

#[test]
fn a_step_that_edits_its_declared_input_is_reported() {
    let project = Project::new(
        r#"
[flow]
name = "self-editing"

[[step]]
id = "edit"
run = "echo changed > spec.md"
inputs = ["spec.md"]
"#,
    );
    project.write("spec.md", "original\n");
    project
        .flot(&["run"])
        .ok()
        .has("the step changed its own declared inputs (spec.md)");
}

const SCOPED: &str = r#"
[flow]
name = "scoped"
max_parallel = 2

[[step]]
id = "edit"
run = "sleep 0.3; echo new > src/lib.txt; mkdir -p out; echo built > out/report.txt"
may_change = ["src/**"]
outputs = ["out/report.txt"]
claims = ["src/lib.txt was rewritten"]
verify = "grep -q new src/lib.txt"

[[step]]
id = "busy"
run = "for i in 1 2 3 4 5 6 7 8 9 10; do echo $i > busy.txt; sleep 0.1; done"
"#;

#[test]
fn a_step_that_changes_files_outside_may_change_fails() {
    let project = Project::new(SCOPED);
    project.write("src/lib.txt", "old\n");
    project.write("notes.txt", "keep\n");
    project.write(".gitignore", "ignored/\n");
    // A step beside it writes all the while; the scoped step runs alone, so
    // none of that is laid to it, and its own output is in scope.
    project.flot(&["run"]).ok().has("claims 1v/0u/0r");
    project
        .flot(&["run", "--only", "edit"])
        .partial()
        .has("cached   edit");
    // A narrower scope did not judge the work, so it does not reuse it.
    project.write("flot.toml", &SCOPED.replace("src/**", "src/lib.txt"));
    project
        .flot(&["run", "--only", "edit"])
        .partial()
        .has("ran      edit");

    // Outside its scope: a new file, an edit and a deletion are all named; a
    // file git ignores is not.
    let strayed = SCOPED.replace(
        "echo built > out/report.txt",
        "echo built > out/report.txt; echo x > stray.txt; echo y >> notes.txt; rm busy.txt; mkdir -p ignored; echo z > ignored/cache",
    );
    project.write("flot.toml", &strayed);
    let run = project.flot(&["run", "--only", "edit"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    run.has("changed files outside may_change: busy.txt, notes.txt, stray.txt")
        .lacks("ignored/cache");

    let invalid = project.flot(&["plan"]);
    invalid.ok();
    project.write("flot.toml", &SCOPED.replace("src/**", "../src/**"));
    project
        .flot(&["plan"])
        .has("must be a relative path inside the flow root");
}

/// Receipt of `step` in the newest run, as `flot receipt` prints it.
fn receipt_of(project: &Project, step: &str) -> serde_json::Value {
    let show = project.flot(&["--json", "show"]).json();
    let receipt_id = show["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["step_id"] == step)
        .and_then(|entry| entry["receipt_id"].as_str())
        .unwrap_or_else(|| panic!("no step {step} in {show}"))
        .to_string();
    project.flot(&["receipt", &receipt_id]).json()
}

const BOUND: &str = r#"
[flow]
name = "bound"

[[step]]
id = "edit"
run = "echo new > src/lib.txt"
inputs = ["task.md"]
may_change = ["src/**"]
claims = ["src/lib.txt says new"]
verify = "grep -q new src/lib.txt"

[[step]]
id = "implement"
# Edits files it does not declare, as an agent step does.
run = "cp task.md src/impl.txt; rm -f src/legacy.txt"
inputs = ["task.md"]
claims = ["src/impl.txt holds the task and src/legacy.txt is gone"]
verify = "grep -q task src/impl.txt && test ! -e src/legacy.txt"

[[step]]
id = "quiet"
run = "echo quiet"
claims = ["it ran"]
verify = "true"
"#;

#[test]
fn a_reused_step_answers_for_the_files_it_changed() {
    let project = Project::new(BOUND);
    project.write("task.md", "task\n");
    project.write("src/lib.txt", "old\n");
    project.write("src/legacy.txt", "legacy\n");
    project.flot(&["run"]).ok().has("PASSED");
    project
        .flot(&["run"])
        .ok()
        .has("cached   edit")
        .has("cached   implement");

    // What a step changed is bound to its receipt, deletions too; a step
    // that changed nothing keeps the receipt it always had.
    let edit = receipt_of(&project, "edit");
    assert_eq!(
        edit["identity"]["changed"],
        serde_json::json!({ "src/lib.txt": flot::core::digest::bytes(b"new\n") }),
        "{edit}"
    );
    let implement = receipt_of(&project, "implement");
    assert_eq!(
        implement["identity"]["changed"]["src/legacy.txt"],
        serde_json::Value::Null,
        "{implement}"
    );
    assert!(implement["identity"]["changed"]
        .as_object()
        .is_some_and(|changed| changed.contains_key("src/legacy.txt")
            && changed.contains_key("src/impl.txt")));
    let quiet = receipt_of(&project, "quiet");
    assert!(
        quiet["identity"].get("changed").is_none(),
        "a step that changed nothing records nothing: {quiet}"
    );

    // Reverting the file the proof judged is not answered by the receipt.
    project.write("src/lib.txt", "old\n");
    project
        .flot(&["run"])
        .ok()
        .has("ran      edit")
        .has("edit: src/lib.txt changed since its receipt; the step runs again")
        .has("cached   implement");
    // Nor is editing a file the step wrote without declaring it.
    project.write("src/impl.txt", "broken\n");
    project
        .flot(&["run"])
        .ok()
        .has("ran      implement")
        .has("implement: src/impl.txt changed since its receipt; the step runs again");
    // Nor bringing back a file it deleted.
    project.write("src/legacy.txt", "legacy\n");
    project.flot(&["run"]).ok().has("ran      implement").has(
        "implement: src/legacy.txt, which the step deleted, is there again; the step runs again",
    );
    project
        .flot(&["run"])
        .ok()
        .has("cached   edit")
        .has("cached   implement")
        .has("cached   quiet");
}

#[test]
fn a_step_that_edits_its_own_input_answers_for_the_edit() {
    // Also where git ignores the input, so only its declaration finds it.
    for ignored in [false, true] {
        let project = Project::new(
            r#"
[flow]
name = "tune"

[[step]]
id = "tune"
run = "echo tuned > config.txt"
inputs = ["config.txt"]
claims = ["config.txt is tuned"]
verify = "grep -q tuned config.txt"
"#,
        );
        if ignored {
            project.write(".gitignore", "config.txt\n");
        }
        project.write("config.txt", "old\n");
        project
            .flot(&["run"])
            .ok()
            .has("the step changed its own declared inputs (config.txt)");
        // Keyed on the tuned file now, it runs once more and then holds.
        project.flot(&["run"]).ok().has("ran      tune");
        project.flot(&["run"]).ok().has("cached   tune");
        // Back to the content the first run was keyed on: its receipt said
        // the step left config.txt tuned, which is no longer so.
        project.write("config.txt", "old\n");
        project
            .flot(&["run"])
            .ok()
            .has("ran      tune")
            .has("tune: config.txt changed since its receipt; the step runs again");
    }
}

#[test]
fn a_later_attempt_answers_for_what_earlier_attempts_changed() {
    let project = Project::new(
        r#"
[flow]
name = "halves"

[[step]]
id = "fix"
attempts = 2
run = """
if [ "$FLOT_ATTEMPT" = 1 ]; then echo half > half.txt; exit 1; fi
echo done > done.txt
"""
claims = ["both halves are there"]
verify = "grep -q half half.txt && grep -q done done.txt"
"#,
    );
    project.flot(&["run"]).ok().has("attempt 2");
    project.flot(&["run"]).ok().has("cached   fix");
    std::fs::remove_file(project.path().join("half.txt")).unwrap();
    project
        .flot(&["run"])
        .ok()
        .has("ran      fix")
        .has("fix: half.txt changed since its receipt; the step runs again");
}
