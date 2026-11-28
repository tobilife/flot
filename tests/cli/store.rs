//! The store: what it keeps, names and sweeps.

use crate::support::{flot_bin, receipt_file, run_id, step_flow, Project, PROVEN, TWO_STEPS};
use std::process::Command;

/// `cp -r`, which a Windows host may not have.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn the_store_is_never_a_step_input_and_the_root_is_not_a_pattern() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("k[1]");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("flot.toml"),
        step_flow(
            "run = \"echo x > out.txt\"\ninputs = [\"**/*\", \".flot/**/*\"]\noutputs = [\"out.txt\"]",
        ),
    )
    .unwrap();
    let flot = |args: &[&str]| {
        let output = Command::new(flot_bin())
            .args(args)
            .current_dir(&root)
            .env("FLOT_SIGNING_KEY", parent.path().join("key"))
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    flot(&["run"]);
    flot(&["run"]);
    let third = flot(&["run"]);
    assert!(third.contains("cached   s"), "{third}");
    assert!(
        third.contains("`.flot/**/*` matched no readable file"),
        "{third}"
    );
}

#[test]
fn a_copied_store_brings_evidence_but_not_decisions() {
    let source = Project::new(PROVEN);
    source.write("seed.txt", "one");
    source.flot(&["key", "init"]).ok();
    let key = source.flot(&["key", "show", "--json"]).json()["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    source.flot(&["trust", "add", "mallory", &key]).ok();
    source.flot(&["run"]).ok().has("ran      write");

    // Another host (its own secret) receives the whole store, cache and
    // trust list included, the way a committed `.flot` would arrive.
    let target = Project::new(PROVEN);
    target.write("seed.txt", "one");
    copy_tree(&source.path().join(".flot"), &target.path().join(".flot"));

    // The trust list is refused, not believed, by everything that would
    // judge authorship with it.
    for command in [&["trust", "list"][..], &["verify", &run_id(&source)][..]] {
        let refused = target.flot(command);
        assert_eq!(refused.code, 1, "{command:?}");
        refused.has("not written by this host");
    }
    std::fs::remove_file(target.path().join(".flot/trust.json")).unwrap();
    // The evidence still audits.
    target
        .flot(&["verify", &run_id(&source)])
        .ok()
        .has("integrity: intact");
    // And the cache does not stand in for running the step here.
    target.flot(&["run"]).ok().has("ran      write");
    target.flot(&["run"]).ok().has("cached   write");
}

#[test]
fn a_run_is_named_by_its_id_or_a_prefix_never_by_a_fragment() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let id = run_id(&project);
    let (stamp, root) = id.split_once('-').unwrap();
    project.flot(&["show", &stamp[..9]]).ok();
    project.flot(&["show", &root[..6]]).ok();
    let fragment = project.flot(&["show", &root[3..9]]);
    assert_eq!(fragment.code, 3);
    fragment.has("no run id or root starts with");
    let short = project.flot(&["show", &root[..2]]);
    assert_eq!(short.code, 3);
    short.has("too short");
}

#[test]
fn a_run_file_holding_another_run_is_refused() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let first = run_id(&project);
    project.write("flot.toml", &TWO_STEPS.replace("echo second", "echo third"));
    project.flot(&["run"]).ok();
    let second = run_id(&project);
    let runs = project.path().join(".flot/runs");
    std::fs::copy(
        runs.join(format!("{second}.json")),
        runs.join(format!("{first}.json")),
    )
    .unwrap();
    let output = project.flot(&["show", &first]);
    assert_eq!(output.code, 1);
    output.has(&format!("holds run {second}"));
}

#[test]
fn reading_a_project_leaves_no_store_behind() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["runs"]).ok().has("no runs recorded");
    project.flot(&["plan"]).ok();
    project.flot(&["gc"]).ok();
    assert!(!project.path().join(".flot").exists());
}

#[test]
fn gc_sweeps_what_only_describes_a_removed_run() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["key", "init"]).ok();
    project.flot(&["run"]).ok();
    project.flot(&["bundle", "--sign"]).ok();
    let id = run_id(&project);
    let attestation = project.path().join(format!(".flot/attestations/{id}.json"));
    assert!(attestation.exists());
    std::fs::remove_file(project.path().join(format!(".flot/runs/{id}.json"))).unwrap();
    std::fs::create_dir_all(project.path().join(".flot/tmp")).unwrap();
    std::fs::write(project.path().join(".flot/tmp/left-over"), "x").unwrap();
    project.flot(&["gc"]).ok();
    assert!(!attestation.exists());
    assert!(!project.path().join(".flot/tmp/left-over").exists());
}

#[test]
fn an_unreadable_run_is_named_and_does_not_hide_the_others() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let broken = run_id(&project);
    project.write("flot.toml", &TWO_STEPS.replace("echo second", "echo third"));
    project.flot(&["run"]).ok();
    let good = run_id(&project);
    project.write(&format!(".flot/runs/{broken}.json"), "{");

    let runs = project.flot(&["runs"]);
    assert_eq!(runs.code, 1, "stdout:\n{}", runs.stdout);
    runs.has(&good).has(&format!("unreadable  {broken}"));

    // gc cannot know what the broken run references, so it collects nothing.
    let gc = project.flot(&["gc"]);
    assert_eq!(gc.code, 1);
    gc.has("gc stopped").has(&format!("run {broken}"));

    // Nor what a broken receipt references.
    std::fs::remove_file(project.path().join(format!(".flot/runs/{broken}.json"))).unwrap();
    std::fs::write(receipt_file(&project, 0), "{").unwrap();
    let gc = project.flot(&["gc"]);
    assert_eq!(gc.code, 1);
    gc.has("gc stopped").has("receipt sha256:");
}

#[test]
fn a_clock_set_back_does_not_put_a_run_behind_the_one_before_it() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    // A run an hour ahead of this clock, as a clock set back leaves one.
    let store = project.path().join(".flot");
    let latest = std::fs::read_to_string(store.join("latest")).unwrap();
    let runs = store.join("runs");
    let mut manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(runs.join(format!("{latest}.json"))).unwrap(),
    )
    .unwrap();
    let ahead = manifest["started_at_ms"].as_u64().unwrap() + 3_600_000;
    let root = latest.split_once('-').unwrap().1;
    let id = format!("{ahead:013}-{root}");
    manifest["started_at_ms"] = ahead.into();
    manifest["run_id"] = id.clone().into();
    std::fs::write(runs.join(format!("{id}.json")), manifest.to_string()).unwrap();

    project
        .flot(&["run", "--force"])
        .ok()
        .has("this host's clock read no later than the start of the newest run recorded here");
    let next = format!("{:013}-{root}", ahead + 1);
    assert_eq!(run_id(&project), next);
    let shown = project.flot(&["show", "--json"]).json();
    assert_eq!(shown["run_id"], next.as_str());
    assert_eq!(
        project.flot(&["show", "--json", "latest~1"]).json()["run_id"],
        id.as_str()
    );
    project.flot(&["verify"]).ok();

    // Another host's clock is its own: a run imported from ahead of this
    // clock moves nothing here.
    let elsewhere = Project::new(TWO_STEPS);
    elsewhere.flot(&["run"]).ok();
    let runs = elsewhere.path().join(".flot/runs");
    std::fs::write(runs.join(format!("{id}.json")), manifest.to_string()).unwrap();
    let imported = elsewhere.path().join(".flot/imported");
    std::fs::create_dir_all(&imported).unwrap();
    std::fs::write(imported.join(format!("{id}.json")), "").unwrap();
    elsewhere
        .flot(&["run", "--force"])
        .ok()
        .lacks("this host's clock");
}

#[test]
fn a_new_store_keeps_itself_out_of_git() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let ignore = std::fs::read_to_string(project.path().join(".flot/.gitignore")).unwrap();
    assert!(ignore.lines().any(|line| line == "*"), "{ignore}");
}
