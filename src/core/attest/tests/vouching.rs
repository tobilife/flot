//! Tests of which signers a reader can confirm vouched for a reused step.

use std::collections::BTreeMap;

use crate::core::receipt::{ClaimCounts, RunStep, StepStatus};

use super::super::{fingerprint, public_key_of, sign, vouched, Endorsements, Voucher};
use super::{manifest, OTHER, SECRET};

fn reused(step_id: &str, receipt_id: &str, vouched_by: &[String]) -> RunStep {
    RunStep {
        step_id: step_id.into(),
        receipt_id: receipt_id.into(),
        status: StepStatus::Ok,
        cached: true,
        claims: ClaimCounts::default(),
        observed_digest: String::new(),
        attempts: Vec::new(),
        candidates: Vec::new(),
        vouched_by: vouched_by.to_vec(),
    }
}

#[test]
fn a_signer_is_confirmed_only_by_a_run_it_signed_that_kept_the_same_receipt() {
    let alice = public_key_of(&SECRET);
    let stranger = public_key_of(&OTHER);
    let mut signed = manifest("sha256:signed");
    signed.steps = vec![reused("build", "sha256:r1", &[])];
    let mut ci = manifest("sha256:ci");
    ci.steps = vec![
        reused("build", "sha256:r1", &[alice.clone(), stranger.clone()]),
        // Alice signed this receipt for another step, and this step
        // with another receipt: neither is what she vouched for.
        reused("test", "sha256:r1", std::slice::from_ref(&alice)),
    ];
    let mut other_receipt = manifest("sha256:other");
    other_receipt.steps = vec![reused("build", "sha256:r9", std::slice::from_ref(&alice))];
    let trusted = BTreeMap::from([(alice.clone(), "alice".to_string())]);

    // A run that names alice is not confirmed by naming her, nor by an
    // attestation of hers that does not hold for the run it came with.
    let mut endorsements = Endorsements::default();
    endorsements.add(&[sign(&SECRET, &manifest("sha256:else"))], &signed);
    let unconfirmed = vouched(&ci, &trusted, &endorsements);
    assert_eq!(
        unconfirmed[0].vouched_by[0],
        Voucher {
            key: alice.clone(),
            confirmed: false,
            trusted_as: None,
        }
    );
    assert_eq!(
        unconfirmed[0].vouched_by[0].label(),
        format!("{} (not confirmed here)", fingerprint(&alice))
    );

    endorsements.add(&[sign(&SECRET, &signed)], &signed);
    let judged = vouched(&ci, &trusted, &endorsements);
    assert_eq!(judged.len(), 2);
    assert_eq!(judged[0].vouched_by[0].label(), "alice");
    assert!(!judged[0].vouched_by[1].confirmed, "{judged:?}");
    assert!(!judged[1].vouched_by[0].confirmed, "{judged:?}");
    let elsewhere = vouched(&other_receipt, &trusted, &endorsements);
    assert!(!elsewhere[0].vouched_by[0].confirmed, "{elsewhere:?}");

    // Confirmed, then no longer trusted: named by key, and said so.
    let revoked = vouched(&ci, &BTreeMap::new(), &endorsements);
    assert_eq!(
        revoked[0].vouched_by[0].label(),
        format!("{} (not trusted here)", fingerprint(&alice))
    );
}
