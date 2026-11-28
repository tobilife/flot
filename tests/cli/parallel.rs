//! Independent steps running side by side.

use crate::support::{Project, TWO_STEPS};

const FAN_OUT: &str = r#"
[flow]
name = "fan-out"

[[step]]
id = "seed"
run = "echo seed"

[[step]]
id = "left"
run = "sleep 0.6; echo left"
needs = ["seed"]

[[step]]
id = "right"
run = "sleep 0.6; echo right"
needs = ["seed"]

[[step]]
id = "join"
run = "echo join"
needs = ["left", "right"]
claims = ["both branches finished before the join"]
verify = "true"
"#;

fn run_duration_ms(project: &Project, args: &[&str]) -> u64 {
    let output = project.flot(args);
    output.ok();
    output.json()["manifest"]["duration_ms"].as_u64().unwrap()
}

#[test]
fn independent_steps_overlap_only_when_concurrency_is_allowed() {
    let sequential = Project::new(FAN_OUT);
    let serial_ms = run_duration_ms(&sequential, &["run", "--json"]);
    assert!(
        serial_ms >= 1200,
        "one job must not overlap the two 600ms branches, took {serial_ms}ms"
    );

    Project::new(RENDEZVOUS).flot(&["run", "--jobs", "2"]).ok();
}

/// Each branch waits until the other has started, so the run passes only if
/// they overlap, however slowly processes start on the host.
const RENDEZVOUS: &str = r#"
[flow]
name = "rendezvous"

[[step]]
id = "left"
run = "touch left.here; n=0; until [ -e right.here ]; do n=$((n+1)); [ $n -lt 200 ]; sleep 0.05; done"

[[step]]
id = "right"
run = "touch right.here; n=0; until [ -e left.here ]; do n=$((n+1)); [ $n -lt 200 ]; sleep 0.05; done"
"#;

#[test]
fn concurrency_does_not_change_receipts_or_the_run_root() {
    let sequential = Project::new(FAN_OUT);
    let serial = sequential.flot(&["run", "--json"]);
    serial.ok();

    let parallel = Project::new(FAN_OUT);
    let concurrent = parallel.flot(&["run", "--jobs", "4", "--json"]);
    concurrent.ok();

    assert_eq!(
        serial.json()["manifest"]["root_digest"],
        concurrent.json()["manifest"]["root_digest"],
        "the same work must fold to the same root at any concurrency"
    );
    let serial_steps: Vec<String> = serial.json()["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["step_id"].as_str().unwrap().to_string())
        .collect();
    let concurrent_steps: Vec<String> = concurrent.json()["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["step_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        serial_steps, concurrent_steps,
        "steps are reported in dependency order, not completion order"
    );
}

#[test]
fn a_parallel_run_states_what_concurrency_does_not_prove() {
    let project = Project::new(FAN_OUT);
    project
        .flot(&["run", "--jobs", "2"])
        .ok()
        .has("ran concurrently");
}

#[test]
fn max_parallel_in_the_flow_file_enables_overlap_without_a_flag() {
    Project::new(&RENDEZVOUS.replace(
        "name = \"rendezvous\"",
        "name = \"rendezvous\"\nmax_parallel = 2",
    ))
    .flot(&["run"])
    .ok();
}

#[test]
fn a_failing_branch_cancels_its_own_dependents_while_the_other_branch_completes() {
    let project = Project::new(
        r#"
[flow]
name = "half-broken"
max_parallel = 4

[[step]]
id = "broken"
run = "exit 1"

[[step]]
id = "after-broken"
run = "touch must-not-exist.txt"
needs = ["broken"]

[[step]]
id = "healthy"
run = "touch healthy-ran.txt"
"#,
    );
    let output = project.flot(&["run", "--json"]);
    assert_eq!(output.code, 1);
    let statuses: Vec<(String, String)> = output.json()["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            (
                step["step_id"].as_str().unwrap().to_string(),
                step["status"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(statuses.contains(&("broken".into(), "failed".into())));
    assert!(statuses.contains(&("after-broken".into(), "cancelled".into())));
    assert!(statuses.contains(&("healthy".into(), "ok".into())));
    assert!(project.path().join("healthy-ran.txt").exists());
    assert!(!project.path().join("must-not-exist.txt").exists());
}

#[test]
fn zero_jobs_is_refused() {
    let project = Project::new(TWO_STEPS);
    assert_eq!(project.flot(&["run", "-j", "0"]).code, 3);
}
