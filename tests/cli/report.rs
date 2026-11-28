//! What `show`, `diff` and the reports say about a run.

use crate::support::{receipt_file, run_id, step_flow, Project, PROVEN, TWO_STEPS};

#[test]
fn diff_reports_identical_runs_and_real_changes() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    let runs = project.flot(&["runs", "--json"]);
    runs.ok();
    let first_id = runs.json()["runs"][0]["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    project.flot(&["run"]).ok();
    project
        .flot(&["diff", &first_id, "latest"])
        .ok()
        .has("identical");

    project.write("seed.txt", "changed");
    project.flot(&["run"]).ok();
    let changed = project.flot(&["diff", &first_id, "latest"]);
    assert_eq!(changed.code, 2);
    changed
        .has("receipt changed")
        .has("write")
        .has("        inputs: seed.txt changed\n");
    let changed = project.flot(&["diff", &first_id, "latest", "--json"]);
    assert_eq!(
        changed.json()["steps"][0]["reasons"],
        serde_json::json!(["inputs: seed.txt changed"])
    );

    // An altered receipt says nothing about why.
    let path = receipt_file(&project, 0);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replacen("echo flot", "echo forged", 1)).unwrap();
    let altered = project.flot(&["diff", &first_id, "latest"]);
    assert_eq!(altered.code, 2);
    altered
        .has("a receipt is not in this store, so why cannot be told")
        .lacks("command changed");
}

#[test]
fn diff_says_the_flow_definition_differs_only_when_it_does() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let first = run_id(&project);
    project.flot(&["run", "--force"]).ok();
    let second = run_id(&project);
    let same = project.flot(&["diff"]);
    assert_eq!(
        same.ok().stdout,
        format!(
            "left  {first}  passed\nright {second}  passed\nidentical: same root digest, step for step\n"
        )
    );
    project.write(
        "flot.toml",
        &TWO_STEPS.replace("echo second", "echo changed"),
    );
    project.flot(&["run"]).ok();
    let changed = project.flot(&["diff"]);
    assert_eq!(changed.code, 2, "{}", changed.stdout);
    changed.has("  passed\nflow definition differs between the two runs\n");
}

#[test]
fn diff_names_a_partial_run_and_the_steps_it_left_out() {
    let project = Project::new(TWO_STEPS);
    assert_eq!(project.flot(&["run", "--only", "a"]).code, 2);
    project.flot(&["run"]).ok();
    let diff = project.flot(&["diff"]);
    assert_eq!(diff.code, 2, "{}", diff.stdout);
    diff.has("passed, partial: 1 of 2 steps")
        .has("  b                    not selected in left")
        .lacks("added");
    let diff = project.flot(&["diff", "--json"]).json();
    assert_eq!(
        diff["left_partial"],
        serde_json::json!({"selected": 1, "of": 2})
    );
    assert!(diff.get("right_partial").is_none(), "{diff}");
}

#[test]
fn json_output_is_valid_json_whatever_a_step_prints() {
    // DEL, a C1 control (CSI) and a bidi isolate: none are escaped by JSON
    // itself, and each must reach a JSON reader intact.
    let project = Project::new(&step_flow(
        "run = \"printf 'x\\\\177y\\\\302\\\\233z\\\\342\\\\201\\\\250w'\"",
    ));
    let output = project.flot(&["run", "--json"]);
    output.ok();
    let report = output.json();
    assert_eq!(
        report["steps"][0]["stdout_inline"], "x\u{7f}y\u{9b}z\u{2068}w",
        "{}",
        output.stdout
    );
    assert!(
        !output.stdout.contains('\u{9b}'),
        "the terminal never sees CSI"
    );
}

#[test]
fn show_json_keeps_claim_totals_and_claims_per_step_apart() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    let output = project.flot(&["show", "--json"]);
    assert_eq!(output.stdout.matches("\n  \"claims\"").count(), 1);
    let detail = output.json();
    assert_eq!(detail["claims"]["verified"], 1);
    assert_eq!(detail["step_claims"]["write"][0]["status"], "verified");
}

#[test]
fn a_reused_step_is_one_line_its_output_stays_in_the_store() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok().has("    | first");
    project
        .flot(&["run"])
        .ok()
        .has("cached   a")
        .lacks("    | first")
        .lacks("    | second");
}

#[test]
fn diff_compares_the_latest_run_with_the_one_before_by_default() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let first = run_id(&project);
    project.flot(&["run", "--force"]).ok();
    let second = run_id(&project);
    assert_ne!(first, second);
    let diff = project.flot(&["diff", "--json"]).json();
    assert_eq!(diff["left"], first.as_str(), "{diff}");
    assert_eq!(diff["right"], second.as_str(), "{diff}");
    let shown = project.flot(&["show", "--json", "latest~1"]).json();
    assert_eq!(shown["run_id"], first.as_str());

    let far = project.flot(&["show", "latest~2"]);
    assert_eq!(far.code, 3, "stderr:\n{}", far.stderr);
    far.has("goes back further than the 2 run(s)");
    let garbled = project.flot(&["show", "latest~one"]);
    assert_eq!(garbled.code, 3, "stderr:\n{}", garbled.stderr);
    garbled.has("not a run selector");
}

#[test]
fn a_limitation_shared_by_several_steps_is_one_line() {
    let project = Project::new(TWO_STEPS);
    let run = project.flot(&["run"]);
    run.ok().has("  - a, b: step declares no inputs");
    assert_eq!(run.stdout.matches("step declares no inputs").count(), 1);
    project
        .flot(&["show"])
        .ok()
        .has("  - a, b: step declares no inputs");
    // The structured reply keeps one entry per step.
    let manifest = project.flot(&["show", "--json"]).json();
    let per_step = manifest["limitations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry.as_str().unwrap().contains("declares no inputs"))
        .count();
    assert_eq!(per_step, 2);
}

/// Two steps that both produce two files whose names end alike after a
/// colon, so that the run's two conflicts read `<one thing>: <same text>`.
#[cfg(unix)]
const CLASHING: &str = r#"
[flow]
name = "clash"

[[step]]
id = "a"
run = "echo a > 'x: out'; echo a > 'y: out'"
inputs = ["flot.toml"]
outputs = ["x: out", "y: out"]

[[step]]
id = "b"
run = "echo b > 'x: out'; echo b > 'y: out'"
inputs = ["flot.toml"]
outputs = ["x: out", "y: out"]
"#;

#[cfg(unix)]
#[test]
fn limitations_of_the_whole_run_that_end_alike_are_not_grouped_as_a_steps() {
    let project = Project::new(CLASHING);
    let run = project.flot(&["run"]);
    assert!(
        run.ok().stdout.ends_with(
            "\nlimitations:\n  - steps a and b both produce x: out and neither needs the other; which content survives depends on scheduling\n  - steps a and b both produce y: out and neither needs the other; which content survives depends on scheduling\n"
        ),
        "{}",
        run.stdout
    );
}

/// Note `count` refusals for the next run to record, each made just after
/// the latest run ended.
fn note_refusals(project: &Project, count: usize) {
    let latest = project.flot(&["show", "--json"]).json();
    let after =
        latest["started_at_ms"].as_u64().unwrap() + latest["duration_ms"].as_u64().unwrap() + 1;
    let notes: String = (1..=count)
        .map(|n| format!("{{\"at_ms\":{after},\"tool\":\"Edit\",\"reasons\":[\"edit {n}\"]}}\n"))
        .collect();
    std::fs::write(project.path().join(".flot/refusals.jsonl"), notes).unwrap();
}

#[test]
fn a_run_lists_twenty_refusals_one_by_one_and_counts_the_rest() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let listed: String = (1..=20).map(|n| format!("  - Edit: edit {n}\n")).collect();
    note_refusals(&project, 20);
    let twenty = project.flot(&["run"]);
    assert!(
        twenty.ok().stdout.ends_with(&format!(
            "\nrefused by the guard since the previous run:\n{listed}"
        )),
        "{}",
        twenty.stdout
    );
    note_refusals(&project, 21);
    let more = project.flot(&["run"]);
    assert!(
        more.ok().stdout.ends_with(&format!(
            "\nrefused by the guard since the previous run:\n{listed}  - and 1 more\n"
        )),
        "{}",
        more.stdout
    );
}

#[test]
fn a_markdown_report_renders_what_a_flow_says_as_text() {
    let project = Project::new(
        r#"
[flow]
name = "report"

[[step]]
id = "write"
run = "echo flot > artifact.txt"
claims = ["see [this](https://example.com) | ![x](y) <b>bold</b>", "artifact.txt holds flot"]
verify = "grep -q flot artifact.txt"
"#,
    );
    project.flot(&["run"]).ok();
    let report = project.flot(&["verify", "--markdown"]);
    report
        .ok()
        .has("### flot: passed")
        .has("- Integrity: intact")
        .has("| `write` | ok | see \\[this\\]\\(https://example\\.com\\) \\| \\!\\[x\\]\\(y\\) &lt;b\\>bold&lt;/b\\> | verified |")
        .has("|  |  | artifact\\.txt holds flot | verified |")
        .lacks("Proofs:");
    let both = project.flot(&["--json", "verify", "--markdown"]);
    assert_eq!(both.code, 3, "{}", both.stderr);
}
