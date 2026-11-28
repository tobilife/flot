//! Why a run did not pass, as `flot explain` prints it.

use crate::support::Project;

const BROKEN: &str = r#"
[flow]
name = "broken"

[[step]]
id = "build"
run = "echo compiling; exit 3"

[[step]]
id = "test"
run = "true"
needs = ["build"]
claims = ["the tests pass"]
verify = "true"

[[step]]
id = "docs"
run = "true"
claims = ["the docs build"]
verify = "echo missing page; exit 1"
"#;

#[test]
fn explain_names_each_cause_and_the_command_to_read_next() {
    let project = Project::new(BROKEN);
    project.flot(&["run"]);
    let explained = project.flot(&["explain"]);
    assert_eq!(explained.code, 1, "{}", explained.stdout);
    explained
        .has("FAILED ")
        .has("build                failed    ran\n  - failed: its command exited 3\n    next: flot log build --stderr\n")
        .has("  - refuted: its proof exited 1\n    next: flot log docs --proof\n")
        .has("  - not run: dependency build (failed) did not hold\n    next: flot explain\n")
        .lacks("nothing to explain");

    // One step, whether it held or not; the same over --json.
    let one = project.flot(&["explain", "latest", "docs"]);
    one.has("docs ").lacks("build ");
    let json = project
        .flot(&["--json", "explain", "latest", "docs"])
        .json();
    assert_eq!(json["steps"][0]["causes"][0]["kind"], "refuted", "{json}");
    assert_eq!(
        json["steps"][0]["causes"][0]["next"],
        "flot log docs --proof"
    );
    let unknown = project.flot(&["explain", "latest", "deploy"]);
    assert_eq!(unknown.code, 3, "{}", unknown.stderr);
    assert!(unknown
        .stderr
        .contains("has no step `deploy`; its steps are build, docs, test"));

    // A run that passed has nothing to explain.
    project.write(
        "flot.toml",
        &BROKEN.replace("exit 3", "true").replace("exit 1", "true"),
    );
    project.flot(&["run"]).ok();
    project
        .flot(&["explain"])
        .ok()
        .has("PASSED ")
        .has("nothing to explain: every step held");
}
