//! Tests of how the dependency receipts a run's receipts name are judged.

use std::collections::BTreeMap;

use crate::core::receipt::{verdict_for, DepRef, Observed, Receipt, RunManifest};

use super::{coherent, identity, judge_all};

/// Step `b` of this demo flow, built on the receipt `a` for step `a`.
fn built_on(a: &Receipt) -> Receipt {
    let mut body = identity("b", "out");
    body.deps = vec![DepRef {
        step_id: "a".into(),
        receipt_id: a.receipt_id.clone(),
    }];
    Receipt::seal(body, Observed::default())
}

/// A run of `a` and then `b` whose manifest records `a` and `b`, with the
/// one observation both steps share.
fn run_of(a: &Receipt, b: &Receipt) -> (RunManifest, BTreeMap<String, Observed>) {
    let mut manifest = coherent(b);
    let mut first = manifest.steps[0].clone();
    first.step_id = "a".into();
    first.receipt_id = a.receipt_id.clone();
    first.claims = a.claim_counts();
    manifest.steps.insert(0, first);
    let pairs: Vec<(String, String)> = manifest
        .steps
        .iter()
        .map(|step| (step.step_id.clone(), step.receipt_id.clone()))
        .collect();
    manifest.root_digest = RunManifest::root_of(&pairs);
    manifest.run_id = RunManifest::id_for(manifest.started_at_ms, &manifest.root_digest);
    manifest.spend.add_executed(Observed::default().metrics);
    manifest.verdict = verdict_for(&manifest.steps, manifest.claims);
    let observations = BTreeMap::from([(
        manifest.steps[0].observed_digest.clone(),
        Observed::default(),
    )]);
    (manifest, observations)
}

#[test]
fn a_receipt_must_be_built_on_the_dependency_receipt_this_run_recorded() {
    let a = Receipt::seal(identity("a", "out"), Observed::default());
    let b = built_on(&a);
    let (manifest, observations) = run_of(&a, &b);
    assert_eq!(
        judge_all(&manifest, &[&a, &b], &observations),
        Vec::<String>::new()
    );

    // `b` was built on another receipt for `a` than the one the run kept.
    let elsewhere = Receipt::seal(identity("a", "other"), Observed::default());
    let stale = built_on(&elsewhere);
    let (manifest, observations) = run_of(&a, &stale);
    assert_eq!(
        judge_all(&manifest, &[&a, &stale], &observations),
        vec!["b: references a different receipt for `a` than the run recorded".to_string()]
    );
}
