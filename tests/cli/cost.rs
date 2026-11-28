//! Spend reported by steps and the ceiling it is held to.

use crate::support::{Project, PROVEN};

fn billed(id: &str, dollars: &str) -> String {
    format!(
        "[[step]]\nid = \"{id}\"\nrun = \"echo '{{\\\"input_tokens\\\": 100, \\\"output_tokens\\\": 10, \\\"cost_usd\\\": {dollars}}}' > \\\"$FLOT_METRICS\\\"\"\n\n"
    )
}

fn billed_flow(name: &str, steps: &[(&str, &str)]) -> String {
    let mut flow = format!("[flow]\nname = \"{name}\"\n\n");
    for (id, dollars) in steps {
        flow.push_str(&billed(id, dollars));
    }
    flow
}

#[test]
fn steps_report_spend_through_flot_metrics() {
    let project = Project::new(&billed_flow("billed", &[("a", "0.25"), ("b", "0.5")]));
    let run = project.flot(&["run", "--json"]);
    run.ok();
    let spend = &run.json()["manifest"]["spend"];
    assert_eq!(spend["cost_micro_usd"], 750_000);
    assert_eq!(spend["input_tokens"], 200);
    assert_eq!(spend["output_tokens"], 20);
    assert_eq!(spend["reporting_steps"], 2);
    assert_eq!(spend["silent_steps"], 0);
    assert_eq!(run.json()["steps"][0]["metrics"]["cost_micro_usd"], 250_000);
    project
        .flot(&["run", "--force"])
        .ok()
        .has("spent $0.750000");
}

#[test]
fn the_cost_ceiling_stops_new_steps_once_reported_spend_reaches_it() {
    let project = Project::new(&billed_flow(
        "ceiling",
        &[("a", "0.6"), ("b", "0.6"), ("c", "0.6")],
    ));
    let output = project.flot(&["run", "--max-cost", "1.0", "--json"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    let statuses: Vec<String> = output.json()["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["status"].as_str().unwrap().to_string())
        .collect();
    // a spends 0.6; b starts below the ceiling and spends 0.6; c is refused.
    assert_eq!(statuses, vec!["ok", "ok", "cancelled"]);
    let limitations = output.json()["manifest"]["limitations"].to_string();
    assert!(
        limitations.contains("cost ceiling of $1.000000 reached"),
        "{limitations}"
    );
    assert!(
        limitations.contains("spent $1.200000 against a $1.000000 ceiling"),
        "{limitations}"
    );
}

#[test]
fn reused_receipts_cost_nothing_and_pass_under_any_ceiling() {
    let project = Project::new(&billed_flow("reuse", &[("a", "0.6"), ("b", "0.6")]));
    project.flot(&["run"]).ok();
    let reused = project.flot(&["run", "--max-cost", "0.01", "--json"]);
    reused.ok();
    let spend = &reused.json()["manifest"]["spend"];
    assert_eq!(spend["cost_micro_usd"], 0);
    assert_eq!(spend["reused_cost_micro_usd"], 1_200_000);
}

#[test]
fn a_ceiling_names_steps_whose_spend_it_cannot_see() {
    let project = Project::new(
        r#"
[flow]
name = "silent"
max_cost_usd = 5

[[step]]
id = "quiet"
run = "echo no metrics here"
"#,
    );
    project
        .flot(&["run"])
        .ok()
        .has("1 executed step(s) reported no metrics (quiet)");
}

#[test]
fn a_malformed_metrics_file_is_reported_and_treated_as_unknown_spend() {
    let project = Project::new(
        r#"
[flow]
name = "garbled"

[[step]]
id = "garbled"
run = "echo '{\"cost_usd\": \"lots\"}' > \"$FLOT_METRICS\""
"#,
    );
    let run = project.flot(&["run", "--json"]);
    run.ok();
    assert_eq!(run.json()["manifest"]["spend"]["silent_steps"], 1);
    assert!(run.json()["manifest"]["limitations"]
        .to_string()
        .contains("metrics file ignored"));
}

#[test]
fn an_invalid_cost_ceiling_is_refused() {
    let project = Project::new(PROVEN);
    assert_eq!(project.flot(&["run", "--max-cost", "-1"]).code, 3);
    let bad_flow =
        Project::new(&PROVEN.replace("name = \"proven\"", "name = \"proven\"\nmax_cost_usd = -2"));
    let output = bad_flow.flot(&["run"]);
    assert_eq!(output.code, 3);
    output.has("max_cost_usd");
}

#[test]
fn hostile_metrics_never_crash_or_hang_a_run() {
    // A FIFO would block a reader forever; Windows has none, and a directory
    // is not a regular file either.
    #[cfg(unix)]
    const NOT_A_FILE: &str = "mkfifo";
    #[cfg(not(unix))]
    const NOT_A_FILE: &str = "mkdir";
    let project = Project::new(
        &"[flow]\nname = \"m\"\n\n[[step]]\nid = \"a\"\nrun = \"echo '{\\\"input_tokens\\\": 18446744073709551615}' > \\\"$FLOT_METRICS\\\"\"\n\n[[step]]\nid = \"b\"\nrun = \"echo '{\\\"input_tokens\\\": 18446744073709551615}' > \\\"$FLOT_METRICS\\\"\"\n\n[[step]]\nid = \"c\"\nrun = \"{SPECIAL} \\\"$FLOT_METRICS\\\"\"\n"
            .replace("{SPECIAL}", NOT_A_FILE),
    );
    project
        .flot(&["run"])
        .ok()
        .has("c: metrics file ignored: it is not a regular file");
}
