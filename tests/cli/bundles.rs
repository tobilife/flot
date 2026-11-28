//! Moving a run to another host in a bundle.

use crate::support::{run_id, Project, PROVEN};

#[test]
fn a_bundle_moves_a_run_to_another_host_and_still_authenticates() {
    let source = Project::new(PROVEN);
    source.write("seed.txt", "one");
    source.flot(&["run"]).ok();
    let bundle_path = source.path().join("run.flotbundle.json");
    source
        .flot(&["bundle", "-o", bundle_path.to_str().unwrap()])
        .ok()
        .has("receipts");

    let run_id = run_id(&source);

    let target = Project::new(PROVEN);
    target
        .flot(&["import", bundle_path.to_str().unwrap()])
        .ok()
        .has("imported run");
    target
        .flot(&["verify", &run_id])
        .ok()
        .has("integrity: intact")
        .has("verdict passed");
    target.flot(&["runs"]).ok().has("(imported)");
    let runs = target.flot(&["runs", "--json"]);
    assert_eq!(runs.json()["runs"][0]["imported"], true);

    // `latest` means work done here; someone else's run is never the default.
    let bare = target.flot(&["verify"]);
    assert_eq!(bare.code, 3);
    bare.has("no run was executed here yet");
    target.flot(&["run"]).ok();
    let mine = target.flot(&["show", "--json"]).json();
    assert_ne!(mine["run_id"], run_id.as_str());
    assert_eq!(mine["imported"], false);

    // Importing the same bundle again changes nothing and does not move `latest`.
    target.flot(&["import", bundle_path.to_str().unwrap()]).ok();
    assert_eq!(
        target.flot(&["show", "--json"]).json()["run_id"],
        mine["run_id"]
    );

    // A host signs only what it ran.
    target.flot(&["key", "init"]).ok();
    let resign = target.flot(&["bundle", "--sign", &run_id]);
    assert_eq!(resign.code, 3);
    resign.has("can be bundled again but not signed");
}

#[test]
fn a_forged_bundle_is_refused_and_leaves_the_store_empty() {
    let source = Project::new(PROVEN);
    source.write("seed.txt", "one");
    source.flot(&["run"]).ok();
    let bundle_path = source.path().join("run.flotbundle.json");
    source
        .flot(&["bundle", "-o", bundle_path.to_str().unwrap()])
        .ok();

    let text = std::fs::read_to_string(&bundle_path).unwrap();
    let forged = text.replacen("grep -q flot artifact.txt", "true", 1);
    assert_ne!(text, forged, "the forgery must change the bundle");
    std::fs::write(&bundle_path, forged).unwrap();

    let target = Project::new(PROVEN);
    let output = target.flot(&["import", bundle_path.to_str().unwrap()]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.has("does not match its id");
    target.flot(&["runs"]).ok().has("no runs recorded");
}

#[test]
fn a_bundle_cannot_name_a_run_outside_the_store() {
    let source = Project::new(PROVEN);
    source.write("seed.txt", "one");
    source.flot(&["run"]).ok();
    let path = source.path().join("run.flotbundle.json");
    source.flot(&["bundle", "-o", path.to_str().unwrap()]).ok();
    let mut bundle: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    bundle["run"]["run_id"] = "../../../escaped".into();
    std::fs::write(&path, bundle.to_string()).unwrap();

    let target = Project::new(PROVEN);
    let output = target.flot(&["import", path.to_str().unwrap()]);
    assert_eq!(output.code, 1);
    output.has("is not derived from its start time and root");
    assert!(!target.path().join("..").join("escaped.json").exists());
}
