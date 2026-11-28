//! What a markdown report says of spend and of the guard's refusals.

use crate::support::{step_flow, Project};

/// Spend is stated of a run in which a step reported some, and of no other.
#[test]
fn a_markdown_report_states_spend_only_when_a_step_reported_some() {
    let billed = Project::new(&step_flow(
        "run = \"echo '{\\\"cost_usd\\\": 0.25}' > \\\"$FLOT_METRICS\\\"\"",
    ));
    billed.flot(&["run"]).ok();
    billed
        .flot(&["verify", "--markdown"])
        .ok()
        .has("- Claims: 0 verified, 0 unverified, 0 refuted\n- Spend: $0.250000\n\n| Step");
    let silent = Project::new(&step_flow("run = \"true\""));
    silent.flot(&["run"]).ok();
    silent
        .flot(&["verify", "--markdown"])
        .ok()
        .has("- Claims: 0 verified, 0 unverified, 0 refuted\n\n| Step")
        .lacks("Spend");
}

/// Twenty refusals of the guard are each listed; past twenty, the rest are
/// counted.
#[test]
fn a_markdown_report_lists_twenty_refusals_and_counts_the_rest() {
    let project = Project::new(&step_flow("run = \"true\""));
    project.flot(&["run"]).ok();
    for (count, rest) in [(20, ""), (21, "  - and 1 more\n")] {
        // Noted after the previous run ended, for the next one to record.
        let previous = project.flot(&["show", "--json"]).json();
        let at = previous["started_at_ms"].as_u64().unwrap()
            + previous["duration_ms"].as_u64().unwrap()
            + 1;
        let notes: String = (0..count)
            .map(|n| {
                format!("{{\"at_ms\":{at},\"tool\":\"Edit\",\"reasons\":[\"f{n} is held\"]}}\n")
            })
            .collect();
        project.write(".flot/refusals.jsonl", &notes);
        project.flot(&["run"]).ok();
        let listed: String = (0..20)
            .map(|n| format!("  - Edit: f{n} is held\n"))
            .collect();
        project.flot(&["verify", "--markdown"]).ok().has(&format!(
            "- Refused by the guard since the previous run: {count}\n{listed}{rest}\n| Step"
        ));
    }
}
