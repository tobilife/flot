//! Steps that run every time, and what follows them.

use crate::support::{commit, git_in, Project};

const PROBED: &str = r#"
[flow]
name = "probed"

[[step]]
id = "toolchain"
run = "cat ver"
always = true

[[step]]
id = "build"
needs = ["toolchain"]
run = "echo built"
claims = ["it built"]
verify = "true"
"#;

#[test]
fn an_always_step_runs_every_time_and_its_dependents_follow_its_output() {
    let project = Project::new(PROBED);
    project.write("ver", "1.88\n");
    project
        .flot(&["run"])
        .ok()
        .has("ran      toolchain")
        .has("ran      build");
    let plan = project.flot(&["plan"]);
    plan.ok().has("  run   toolchain");
    // Only `build` is warned that no input keys it: `toolchain` is never
    // reused, whatever its key.
    assert_eq!(
        plan.stdout.matches("step declares no inputs").count(),
        1,
        "{}",
        plan.stdout
    );
    // Same output, same receipt: what needs it is reused.
    project
        .flot(&["run"])
        .ok()
        .has("ran      toolchain")
        .has("cached   build");
    project.write("ver", "1.85\n");
    project
        .flot(&["run"])
        .ok()
        .has("ran      toolchain")
        .has("ran      build");
    project
        .flot(&["run"])
        .ok()
        .has("ran      toolchain")
        .has("cached   build");

    // Candidates would all run every time, and which one is kept could
    // change with what each reported spending.
    let refused = Project::new(&PROBED.replace(
        "always = true",
        "always = true\ncandidates = [\"a\", \"b\"]",
    ))
    .flot(&["plan"]);
    assert_eq!(refused.code, 3, "{}", refused.stderr);
    refused.has("step `toolchain` sets always, which candidates do not support");
}

#[test]
fn plan_predicts_what_follows_an_always_step_on_how_it_came_out_last_time() {
    let project = Project::new(&format!(
        "{}\n[[step]]\nid = \"ship\"\nneeds = [\"build\"]\nrun = \"echo shipped\"\n",
        PROBED.replace(
            "run = \"echo built\"",
            "run = \"echo built\"\ninputs = [\"src.txt\"]"
        )
    ));
    project.write("ver", "1.88\n");
    project.write("src.txt", "one\n");
    let assumes = |args: &[&str]| -> Vec<(String, bool, Vec<String>)> {
        let plan = project.flot(args);
        let plan: serde_json::Value = serde_json::from_str(&plan.ok().stdout).unwrap();
        plan["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| {
                (
                    step["step_id"].as_str().unwrap().to_string(),
                    step["cached"].as_bool().unwrap(),
                    serde_json::from_value(step["assumes"].clone()).unwrap(),
                )
            })
            .collect()
    };
    let none = || Vec::<String>::new();
    let probe = || vec!["toolchain".to_string()];
    // Nothing recorded yet: nothing to predict on.
    assert_eq!(
        assumes(&["plan", "--json"]),
        vec![
            ("toolchain".into(), false, none()),
            ("build".into(), false, none()),
            ("ship".into(), false, none()),
        ]
    );
    project.flot(&["run"]).ok();
    // Reused, and what through: the probe, also for `ship`, which needs it
    // only through `build`.
    assert_eq!(
        assumes(&["plan", "--json"]),
        vec![
            ("toolchain".into(), false, none()),
            ("build".into(), true, probe()),
            ("ship".into(), true, probe()),
        ]
    );
    project
        .flot(&["plan"])
        .ok()
        .has("  run   toolchain")
        .has("  reuse build")
        .has("  reuse ship")
        .has("        ? reused if toolchain comes out as last time\n");
    project
        .flot(&["run"])
        .ok()
        .has("cached   build")
        .has("cached   ship");
    // The plan cannot see what the probe will print; the run can.
    project.write("ver", "1.85\n");
    assert_eq!(
        assumes(&["plan", "--json"])[1],
        ("build".into(), true, probe())
    );
    project
        .flot(&["run"])
        .ok()
        .has("ran      build")
        .has("ran      ship");
    // A step that runs anyway still has its key predicted on the probe, but
    // nothing to say about reuse; what needs it cannot be predicted at all.
    project.write("src.txt", "two\n");
    assert_eq!(
        assumes(&["plan", "--json"]),
        vec![
            ("toolchain".into(), false, none()),
            ("build".into(), false, probe()),
            ("ship".into(), false, none()),
        ]
    );
    project
        .flot(&["plan"])
        .ok()
        .has("  run   build")
        .lacks("? reused");
    // Forced, everything runs and nothing is predicted.
    assert_eq!(
        assumes(&["plan", "--json", "--force"]),
        vec![
            ("toolchain".into(), false, none()),
            ("build".into(), false, none()),
            ("ship".into(), false, none()),
        ]
    );
}

#[test]
fn an_always_step_still_checks_that_its_proof_fails_before_the_change() {
    let project = Project::new(
        "[flow]\nname = \"red\"\nproofs_from = \"main\"\n\n[[step]]\nid = \"check\"\nrun = \"true\"\nalways = true\nclaims = [\"app.txt is fixed\"]\nverify = \"grep -q fixed app.txt\"\nfails_before = true\n",
    );
    project.write("app.txt", "broken\n");
    project.write(".gitignore", ".flot\n.test-keys/\n");
    git_in(project.path(), &["init", "-q", "-b", "main"]);
    commit(project.path(), "base");
    project.write("app.txt", "fixed\n");
    for _ in 0..2 {
        project
            .flot(&["run"])
            .ok()
            .has("ran      check")
            .has("claims 1v/0u/0r");
        project.flot(&["show"]).ok().has("before the change (");
    }
}
