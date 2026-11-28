//! Tests of whether a run's manifest follows from the receipts it names,
//! with the helpers they share.

use std::collections::BTreeMap;

use super::super::tests::identity;
use super::super::*;
use crate::core::metrics::Spend;

mod attempts;
mod candidates;

/// A one-step run whose manifest follows exactly from its receipt.
fn coherent(receipt: &Receipt) -> RunManifest {
    let observed_digest = digest::bytes(b"observation");
    let steps = vec![RunStep {
        observed_digest,
        step_id: receipt.identity.step_id.clone(),
        receipt_id: receipt.receipt_id.clone(),
        status: receipt.identity.status,
        cached: false,
        claims: receipt.claim_counts(),
        attempts: Vec::new(),
        candidates: Vec::new(),
        vouched_by: Vec::new(),
    }];
    let root_digest =
        RunManifest::root_of(&[(steps[0].step_id.clone(), steps[0].receipt_id.clone())]);
    RunManifest {
        run_id: RunManifest::id_for(1, &root_digest),
        flow: receipt.identity.flow.clone(),
        flow_digest: String::new(),
        root_digest,
        verdict: verdict_for(&steps, receipt.claim_counts()),
        partial: None,
        claims: receipt.claim_counts(),
        limitations: receipt
            .identity
            .limitations
            .iter()
            .map(|limitation| format!("{}: {limitation}", receipt.identity.step_id))
            .collect(),
        steps,
        max_parallel: 1,
        spend: {
            let mut spend = Spend::default();
            spend.add_executed(Observed::default().metrics);
            spend
        },
        max_cost_micro_usd: None,
        started_at_ms: 1,
        duration_ms: 0,
        flot_version: String::new(),
        refusals: Vec::new(),
    }
}

fn judge(manifest: &RunManifest, receipt: &Receipt) -> Vec<String> {
    let receipts = BTreeMap::from([(receipt.receipt_id.as_str(), receipt)]);
    let observations = BTreeMap::from([(
        manifest.steps[0].observed_digest.as_str(),
        Observed::default(),
    )]);
    manifest.disagreements(&receipts, &observations)
}

#[test]
fn a_selection_lists_the_steps_it_names_and_a_signature_covers_it() {
    let receipt = Receipt::seal(identity("a", "out"), Observed::default());
    let mut partial = coherent(&receipt);
    partial.partial = Some(Partial { selected: 1, of: 2 });
    assert_eq!(judge(&partial, &receipt), Vec::<String>::new());
    // A run of every step records no selection, so its bytes are as
    // they were before selections were recorded.
    assert!(!serde_json::to_string(&coherent(&receipt))
        .unwrap()
        .contains("partial"));

    for (selected, of, expected) in [
        (1, 1, "partial run of 1 of 1 steps leaves no step out"),
        (2, 3, "partial run of 2 of 3 steps lists 1 step(s)"),
    ] {
        let mut forged = coherent(&receipt);
        forged.partial = Some(Partial { selected, of });
        assert_eq!(judge(&forged, &receipt), vec![expected.to_string()]);
    }

    // Dropping the selection from a signed run breaks the signature.
    let attestation = crate::core::attest::sign(&[7; 32], &partial);
    assert_eq!(crate::core::attest::check(&attestation, &partial), Ok(()));
    let mut whole = partial.clone();
    whole.partial = None;
    assert!(crate::core::attest::check(&attestation, &whole).is_err());
}

#[test]
fn only_a_reused_step_names_its_signers_each_once_by_key() {
    let receipt = Receipt::seal(identity("a", "out"), Observed::default());
    let key = crate::core::attest::public_key_of(&[7; 32]);
    let mut vouched = coherent(&receipt);
    vouched.steps[0].cached = true;
    vouched.steps[0].vouched_by = vec![key.clone()];
    vouched.spend = Spend::default();
    vouched.spend.add_reused(Observed::default().metrics);
    assert_eq!(judge(&vouched, &receipt), Vec::<String>::new());

    let mut executed = coherent(&receipt);
    executed.steps[0].vouched_by = vec![key.clone()];
    let found = judge(&executed, &receipt);
    assert!(found[0].contains("was not reused"), "{found:?}");

    let mut twice = vouched.clone();
    twice.steps[0].vouched_by = vec![key.clone(), key.clone()];
    let found = judge(&twice, &receipt);
    assert!(found[0].contains("named more than once"), "{found:?}");

    let mut shouted = vouched.clone();
    shouted.steps[0].vouched_by = vec![format!("ed25519:{}", key[8..].to_uppercase())];
    let found = judge(&shouted, &receipt);
    assert!(found[0].contains("is not a signer key"), "{found:?}");
}

#[test]
fn a_manifest_must_carry_its_receipts_flow_and_limitations() {
    let mut body = identity("a", "out");
    body.limitations = vec!["inline output trimmed".into()];
    let receipt = Receipt::seal(body, Observed::default());
    let manifest = coherent(&receipt);
    assert_eq!(judge(&manifest, &receipt), Vec::<String>::new());

    let mut cleaner = manifest.clone();
    cleaner.limitations.clear();
    let found = judge(&cleaner, &receipt);
    assert!(
        found[0].contains("omits the receipt's limitation"),
        "{found:?}"
    );

    let mut renamed = manifest.clone();
    renamed.flow = "other".into();
    let found = judge(&renamed, &receipt);
    assert!(found[0].contains("belongs to flow `demo`"), "{found:?}");
}

#[test]
fn a_receipt_must_be_authentic_and_point_at_this_runs_dependencies() {
    let mut body = identity("a", "out");
    body.deps = vec![DepRef {
        step_id: "elsewhere".into(),
        receipt_id: "sha256:y".into(),
    }];
    let receipt = Receipt::seal(body, Observed::default());
    let found = judge(&coherent(&receipt), &receipt);
    assert!(found[0].contains("references `elsewhere`"), "{found:?}");

    let mut edited = Receipt::seal(identity("a", "out"), Observed::default());
    let manifest = coherent(&edited);
    edited.identity.command = "echo other".into();
    let found = judge(&manifest, &edited);
    assert!(found[0].contains("does not match its id"), "{found:?}");
}

fn judge_all(
    manifest: &RunManifest,
    receipts: &[&Receipt],
    observations: &BTreeMap<String, Observed>,
) -> Vec<String> {
    let receipts = receipts
        .iter()
        .map(|receipt| (receipt.receipt_id.as_str(), *receipt))
        .collect();
    let observations = observations
        .iter()
        .map(|(digest, observed)| (digest.as_str(), observed.clone()))
        .collect();
    manifest.disagreements(&receipts, &observations)
}
