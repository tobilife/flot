//! Tests of how the earlier attempts a run made at a step are judged.

use std::collections::BTreeMap;

use crate::core::digest;
use crate::core::metrics::{Metrics, Spend};
use crate::core::receipt::{Observed, Receipt, RunAttempt, RunManifest, StepStatus};

use super::{coherent, identity, judge_all};

/// A run whose final receipt `held` came after one attempt `earlier`,
/// with each attempt's observation and spend.
fn retried(held: &Receipt, earlier: &Receipt) -> (RunManifest, BTreeMap<String, Observed>) {
    let mut manifest = coherent(held);
    let paid = Observed {
        metrics: Some(Metrics {
            cost_micro_usd: Some(7),
            ..Metrics::default()
        }),
        ..Observed::default()
    };
    let paid_digest = digest::bytes(b"paid");
    manifest.steps[0].attempts = vec![RunAttempt {
        receipt_id: earlier.receipt_id.clone(),
        observed_digest: paid_digest.clone(),
    }];
    manifest.spend.add_executed(paid.metrics);
    let observations = BTreeMap::from([
        (
            manifest.steps[0].observed_digest.clone(),
            Observed::default(),
        ),
        (paid_digest, paid),
    ]);
    (manifest, observations)
}

#[test]
fn earlier_attempts_are_judged_with_the_run() {
    let held = Receipt::seal(identity("a", "second"), Observed::default());
    let mut failed = identity("a", "first");
    failed.status = StepStatus::Failed;
    let failed = Receipt::seal(failed, Observed::default());
    let (manifest, observations) = retried(&held, &failed);
    assert_eq!(
        judge_all(&manifest, &[&held, &failed], &observations),
        Vec::<String>::new()
    );

    let found = judge_all(&manifest, &[&held], &observations);
    assert!(found[0].contains("a attempt 1: receipt"), "{found:?}");

    // What an attempt spent is part of what the run spent.
    let mut cheaper = manifest.clone();
    cheaper.spend = coherent(&held).spend;
    let found = judge_all(&cheaper, &[&held, &failed], &observations);
    assert!(found[0].contains("run spend disagrees"), "{found:?}");

    // An attempt that held would have ended the step there.
    let (manifest, observations) = retried(&held, &held);
    let found = judge_all(&manifest, &[&held], &observations);
    assert!(
        found[0].contains("it held, so the step would not have been tried again"),
        "{found:?}"
    );

    // A reused receipt was not attempted in this run at all.
    let (mut manifest, observations) = retried(&held, &failed);
    manifest.steps[0].cached = true;
    manifest.spend = Spend::default();
    manifest.spend.add_reused(Observed::default().metrics);
    manifest
        .spend
        .add_executed(observations[&digest::bytes(b"paid")].metrics);
    let found = judge_all(&manifest, &[&held, &failed], &observations);
    assert!(
        found[0].contains("reused receipt lists attempts"),
        "{found:?}"
    );

    let mut other = identity("b", "first");
    other.status = StepStatus::Failed;
    let other = Receipt::seal(other, Observed::default());
    let (manifest, observations) = retried(&held, &other);
    let found = judge_all(&manifest, &[&held, &other], &observations);
    assert!(
        found[0].contains("receipt belongs to step `b`"),
        "{found:?}"
    );
}
