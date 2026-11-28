//! A bad request and an environment fault, each with its exit code.

use crate::support::{Project, PROVEN, TWO_STEPS};

#[test]
fn a_usage_error_exits_with_the_invalid_request_code_not_unverified() {
    let project = Project::new(PROVEN);
    let output = project.flot(&["run", "--no-such-flag"]);
    assert_eq!(output.code, 3, "exit 2 is reserved for unverified claims");
    assert_eq!(project.flot(&["--help"]).code, 0);
    assert_eq!(project.flot(&["--version"]).code, 0);
}

#[test]
fn an_invalid_flow_is_refused_with_the_request_exit_code() {
    let project = Project::new(
        r#"
[flow]
name = "cyclic"

[[step]]
id = "a"
run = "true"
needs = ["b"]

[[step]]
id = "b"
run = "true"
needs = ["a"]
"#,
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 3);
    output.has("dependency cycle");
}

#[test]
fn an_out_of_range_timestamp_is_refused_not_a_crash() {
    let source = Project::new(PROVEN);
    source.write("seed.txt", "one");
    source.flot(&["run"]).ok();
    let path = source.path().join("run.flotbundle.json");
    source.flot(&["bundle", "-o", path.to_str().unwrap()]).ok();
    let text = std::fs::read_to_string(&path).unwrap();
    let mut bundle: serde_json::Value = serde_json::from_str(&text).unwrap();
    let started = bundle["receipts"][0]["observed"]["started_at_ms"].to_string();
    let forged = text.replacen(
        &format!("\"started_at_ms\": {started}"),
        "\"started_at_ms\": 18446744073709551616",
        1,
    );
    assert_ne!(forged, text, "the field was rewritten");
    bundle = serde_json::Value::Null;
    let _ = bundle;
    std::fs::write(&path, forged).unwrap();
    let target = Project::new(PROVEN);
    let output = target.flot(&["import", path.to_str().unwrap()]);
    assert_eq!(output.code, 3, "stderr:\n{}", output.stderr);
    output.lacks("panicked");
}

// Windows has no mode bits to make a directory unwritable this way.
#[cfg(unix)]
#[test]
fn an_environment_fault_exits_4_not_as_a_bad_request() {
    use std::os::unix::fs::PermissionsExt;
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let runs = project.path().join(".flot/runs");
    std::fs::set_permissions(&runs, std::fs::Permissions::from_mode(0o555)).unwrap();
    let output = project.flot(&["run", "--force"]);
    std::fs::set_permissions(&runs, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(output.code, 4, "stderr:\n{}", output.stderr);
}

#[test]
fn a_missing_file_named_in_a_request_is_a_bad_request() {
    let project = Project::new(TWO_STEPS);
    let output = project.flot(&["import", "absent.flotbundle.json"]);
    assert_eq!(output.code, 3, "stderr:\n{}", output.stderr);
}
