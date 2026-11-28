//! Making a flow file, and finding one from a subdirectory.

use crate::support::{flot_bin, read, Project, PROVEN, TWO_STEPS};
use std::process::Command;

#[test]
fn init_writes_a_runnable_starter_flow() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project { dir };
    std::fs::remove_file(project.path().join("flot.toml")).ok();
    project.flot(&["init"]).ok().has("wrote");
    project.flot(&["run"]).has("inspect");
    project.flot(&["init"]).has("already exists");
}

#[test]
fn the_flow_file_is_found_from_a_subdirectory() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    std::fs::create_dir_all(project.path().join("nested/deep")).unwrap();
    let output = Command::new(flot_bin())
        .arg("run")
        .current_dir(project.path().join("nested/deep"))
        .env("FLOT_SIGNING_KEY", project.key_path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(
        project.path().join(".flot/runs").exists(),
        "the store belongs to the flow root, not the current directory"
    );
}

#[test]
fn a_named_flow_file_is_found_the_same_way_however_it_is_written() {
    let project = Project::new(TWO_STEPS);
    std::fs::create_dir_all(project.path().join("sub")).unwrap();
    std::fs::rename(
        project.path().join("flot.toml"),
        project.path().join("sub/flow.toml"),
    )
    .unwrap();
    project.flot(&["-f", "sub/flow.toml", "run"]).ok();
    let absolute = project.path().join("sub/flow.toml");
    project
        .flot(&["-f", absolute.to_str().unwrap(), "run"])
        .ok()
        .has("cached   a");
    assert!(project.path().join("sub/.flot").exists());
    assert!(!project.path().join(".flot").exists());

    let missing = project.flot(&["-f", "nope.toml", "runs"]);
    assert_eq!(missing.code, 3);
    missing.has("does not exist");
}

#[test]
fn init_in_a_subdirectory_leaves_the_parent_project_alone() {
    let project = Project::new(TWO_STEPS);
    std::fs::create_dir_all(project.path().join("sub")).unwrap();
    let output = Command::new(flot_bin())
        .args(["init", "--force"])
        .current_dir(project.path().join("sub"))
        .env("FLOT_SIGNING_KEY", project.key_path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(read(&project, "flot.toml"), TWO_STEPS);
    assert!(project.path().join("sub/flot.toml").exists());
}
