//! Tests of how the candidate a run kept is judged against the others.

use std::collections::BTreeMap;

use crate::core::digest;
use crate::core::metrics::{Metrics, Spend};
use crate::core::receipt::{Observed, Receipt, RunCandidate, RunManifest, StepStatus};

use super::{coherent, identity, judge_all};

fn candidate(name: &str, stdout: &str, status: StepStatus) -> Receipt {
    let mut body = identity("a", stdout);
    body.status = status;
    body.candidate = Some(name.into());
    Receipt::seal(body, Observed::default())
}

/// A run that kept `kept` over `other`, each having reported a cost.
fn contest(
    kept: &Receipt,
    kept_cost: u64,
    other: &Receipt,
    other_cost: u64,
) -> (RunManifest, BTreeMap<String, Observed>) {
    let mut manifest = coherent(kept);
    let costing = |cost| Observed {
        metrics: Some(Metrics {
            cost_micro_usd: Some(cost),
            ..Metrics::default()
        }),
        ..Observed::default()
    };
    let other_digest = digest::bytes(b"other");
    manifest.steps[0].candidates = vec![RunCandidate {
        name: other.identity.candidate.clone().unwrap(),
        receipt_id: other.receipt_id.clone(),
        observed_digest: other_digest.clone(),
        attempts: Vec::new(),
    }];
    manifest.spend = Spend::default();
    manifest.spend.add_executed(costing(kept_cost).metrics);
    manifest.spend.add_executed(costing(other_cost).metrics);
    let observations = BTreeMap::from([
        (
            manifest.steps[0].observed_digest.clone(),
            costing(kept_cost),
        ),
        (other_digest, costing(other_cost)),
    ]);
    (manifest, observations)
}

#[test]
fn the_kept_candidate_must_follow_from_the_others() {
    let cheap = candidate("cheap", "42", StepStatus::Ok);
    let pricey = candidate("pricey", "42 too", StepStatus::Ok);
    let wrong = candidate("wrong", "41", StepStatus::Failed);

    let (manifest, observations) = contest(&cheap, 1, &pricey, 5);
    assert_eq!(
        judge_all(&manifest, &[&cheap, &pricey], &observations),
        Vec::<String>::new()
    );
    // Keeping the dearer of two that held, or one that failed over one
    // that held, is not what the rule chooses.
    let (manifest, observations) = contest(&pricey, 5, &cheap, 1);
    let found = judge_all(&manifest, &[&cheap, &pricey], &observations);
    assert!(
        found[0].contains("candidate cheap: it ranks above"),
        "{found:?}"
    );
    let (manifest, observations) = contest(&wrong, 0, &cheap, 1);
    let found = judge_all(&manifest, &[&cheap, &wrong], &observations);
    assert!(
        found[0].contains("candidate cheap: it ranks above"),
        "{found:?}"
    );

    // A loser's receipt must be that candidate's.
    let (mut manifest, observations) = contest(&cheap, 1, &pricey, 5);
    manifest.steps[0].candidates[0].name = "other".into();
    let found = judge_all(&manifest, &[&cheap, &pricey], &observations);
    assert!(
        found[0].contains("receipt is not that candidate's"),
        "{found:?}"
    );

    // A kept receipt that names no candidate cannot have rivals.
    let plain = Receipt::seal(identity("a", "plain"), Observed::default());
    let (manifest, observations) = contest(&plain, 1, &pricey, 5);
    let found = judge_all(&manifest, &[&plain, &pricey], &observations);
    assert!(found[0].contains("receipt names none"), "{found:?}");

    // Nor can two entries be the same candidate.
    let again = candidate("cheap", "42 again", StepStatus::Ok);
    let (manifest, observations) = contest(&cheap, 1, &again, 5);
    let found = judge_all(&manifest, &[&cheap, &again], &observations);
    assert!(found[0].contains("appears more than once"), "{found:?}");
}
