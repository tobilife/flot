//! What a run counts as spent: what a step's proof reports, as the step's,
//! and what the run spent against its cost ceiling.

use super::support::{forced, Workspace};
use crate::engine::runner::RunOptions;

/// A proof can spend too, an agent asked to review for instance: what it
/// reports is the step's spend, with what it said about its report.
#[test]
fn what_a_proof_spends_is_its_steps_spend() {
    let workspace = Workspace::new(
        r##"
[flow]
name = "reviewed"

[[step]]
id = "review"
run = '''printf '{"cost_usd": 0.25}' > "$FLOT_METRICS"'''
claims = ["it was reviewed"]
verify = '''printf '{"cost_usd": 0.5, "reviewer": 1}' > "$FLOT_METRICS"'''
"##,
    );
    let report = workspace.run(&RunOptions::default());
    let receipt = workspace.receipt(&report, "review");
    assert_eq!(
        receipt
            .observed
            .metrics
            .and_then(|metrics| metrics.cost_micro_usd),
        Some(750_000)
    );
    assert_eq!(
        receipt.observed.metrics_notes,
        ["verify command: ignored unknown metrics key `reviewer`; known keys are input_tokens, output_tokens, cost_usd"]
    );
    assert_eq!(report.manifest.spend.cost_micro_usd, 750_000);
}

/// The ceiling is how much a run may spend: reaching it is not going past
/// it.
#[test]
fn a_run_that_spends_exactly_its_ceiling_did_not_go_past_it() {
    let workspace = Workspace::new(
        r##"
[flow]
name = "billed"
max_cost_usd = 0.25

[[step]]
id = "bill"
run = '''printf '{"cost_usd": 0.25}' > "$FLOT_METRICS"'''
"##,
    );
    let report = workspace.run(&RunOptions::default());
    assert_eq!(report.manifest.spend.cost_micro_usd, 250_000);
    let told: Vec<&String> = report
        .manifest
        .limitations
        .iter()
        .filter(|limitation| limitation.contains("ceiling"))
        .collect();
    assert!(told.is_empty(), "{told:?}");

    let over = workspace.run(&RunOptions {
        max_cost_usd: Some(0.24),
        ..forced()
    });
    assert!(
        over.manifest
            .limitations
            .iter()
            .any(|limitation| limitation.starts_with("spent $0.250000 against a $0.240000 ceiling")),
        "{:?}",
        over.manifest.limitations
    );
}
