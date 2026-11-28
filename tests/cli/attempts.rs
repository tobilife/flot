//! A step that tries again with what went wrong.

use crate::support::{Project, RETRYING};

#[test]
fn a_step_tries_again_with_what_went_wrong_until_it_holds() {
    let project = Project::new(&RETRYING.replace("GOAL", "2"));
    let run = project.flot(&["run"]);
    run.ok()
        .has("attempt 2 of 3")
        // The second attempt read why the first did not hold, from the
        // recorded evidence: the verdict, the proof's output, its own output.
        .has("flot: attempt 1 of 3 at step `fix` did not hold.")
        .has("  - refuted: count reached the goal")
        .has("count is 1")
        .has("receipt ")
        .has("  attempt 2\n")
        .has("  - fix: took 2 attempts of 3 allowed: 1 refuted, 2 held");
    assert_eq!(
        std::fs::read_to_string(project.path().join("count")).unwrap(),
        "2\n"
    );
    let show = project.flot(&["show"]);
    show.ok().has("      attempt 1  receipt ");
    project
        .flot(&["verify"])
        .ok()
        .has("receipts checked 2")
        .has("integrity: intact");
    // The feedback file is gone once the attempt that read it ended.
    let leftovers: Vec<_> = std::fs::read_dir(project.path().join(".flot/tmp"))
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("feedback-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // What held is reused like any other success.
    project.flot(&["run"]).ok().has("cached   fix");
}

#[test]
fn a_step_that_never_holds_fails_after_its_last_attempt() {
    let project = Project::new(&RETRYING.replace("GOAL", "9"));
    let run = project.flot(&["run"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    run.has("attempt 3 of 3")
        .has("took 3 attempts of 3 allowed: 1 refuted, 2 refuted, 3 refuted")
        .lacks("attempt 4");
}

#[test]
fn a_step_makes_one_attempt_unless_it_allows_more() {
    let project = Project::new(&RETRYING.replace("GOAL", "2").replace("attempts = 3\n", ""));
    let run = project.flot(&["run"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    run.lacks("FLOT_FEEDBACK").lacks("took ");
    assert_eq!(
        std::fs::read_to_string(project.path().join("count")).unwrap(),
        "1\n"
    );
}

#[test]
fn a_failing_command_is_tried_again_too() {
    let project = Project::new(
        "[flow]\nname = \"crash\"\n\n[[step]]\nid = \"a\"\nattempts = 2\ninputs = [\"flot.toml\"]\nrun = \"test -e once || { touch once; exit 7; }\"\n",
    );
    project
        .flot(&["run"])
        .ok()
        .has("a: took 2 attempts of 2 allowed: 1 failed (exit 7), 2 held");
}

#[test]
fn the_cost_ceiling_stops_further_attempts() {
    let project = Project::new(
        &RETRYING
            .replace("GOAL", "9")
            .replace(
                "name = \"retrying\"",
                "name = \"retrying\"\nmax_cost_usd = 1.5",
            )
            .replace(
                "n=$(cat count",
                "echo '{\"cost_usd\": 1.0}' > \"$FLOT_METRICS\"\nn=$(cat count",
            ),
    );
    let run = project.flot(&["run"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    run.has("took 2 attempts of 3 allowed")
        .has("attempt 3 of 3 not run: run cost ceiling of $1.500000 reached ($2.000000 spent by executed steps)")
        .has("spent $2.000000");
}

#[test]
fn attempts_must_allow_at_least_one() {
    let project = Project::new(&RETRYING.replace("attempts = 3", "attempts = 0"));
    let run = project.flot(&["run"]);
    assert_eq!(run.code, 3, "{}", run.stderr);
    run.has("step `fix` attempts must be at least 1");
}

#[test]
fn earlier_attempts_travel_in_a_bundle_and_survive_gc() {
    let project = Project::new(&RETRYING.replace("GOAL", "3"));
    project.flot(&["run"]).ok();
    project.flot(&["gc"]).ok();
    project.flot(&["verify"]).ok().has("receipts checked 3");
    project
        .flot(&["bundle", "-o", "run.json"])
        .ok()
        .has("(3 receipts,");

    let target = Project::new(&RETRYING.replace("GOAL", "3"));
    let bundle = project.path().join("run.json");
    target
        .flot(&["import", bundle.to_str().unwrap()])
        .ok()
        .has("3 receipts");

    // Dropping an attempt's receipt from the bundle is caught.
    let mut altered: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bundle).unwrap()).unwrap();
    altered["receipts"].as_array_mut().unwrap().remove(0);
    std::fs::write(&bundle, altered.to_string()).unwrap();
    let elsewhere = Project::new(&RETRYING.replace("GOAL", "3"));
    let import = elsewhere.flot(&["import", bundle.to_str().unwrap()]);
    assert_eq!(import.code, 1, "{}", import.stdout);
    import.has("bundle carries 2 receipts for a run that names 3");
}
