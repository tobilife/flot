//! Tests of the step-key cache: what makes a hit, what a miss, and the exact
//! bytes of a sealed entry.

use std::fs;

use super::super::tests::{receipt, store};
use crate::core::digest;
use crate::core::receipt::{Observed, Receipt, StepStatus};

#[test]
fn cache_hit_requires_authentic_receipt_and_present_outputs() {
    let (_dir, store) = store();
    let key = digest::bytes(b"step-key");
    let receipt = receipt(&key, b"out");
    store.put_bytes(b"out").unwrap();
    store.put_bytes(b"").unwrap();
    store.put_receipt(&receipt).unwrap();
    store.remember(&key, &receipt.receipt_id, &[]).unwrap();

    let hit = store.recall(&key).unwrap().expect("cache hit");
    assert_eq!(hit.receipt.receipt_id, receipt.receipt_id);
    assert!(hit.vouched_by.is_empty());
}

#[test]
fn cache_misses_when_the_output_object_was_pruned() {
    let (_dir, store) = store();
    let key = digest::bytes(b"step-key");
    let receipt = receipt(&key, b"vanished");
    store.put_receipt(&receipt).unwrap();
    store.remember(&key, &receipt.receipt_id, &[]).unwrap();
    assert!(store.recall(&key).unwrap().is_none());
}

#[test]
fn a_failed_receipt_is_never_a_cache_hit() {
    let (_dir, store) = store();
    let key = digest::bytes(b"step-key");
    let mut body = receipt(&key, b"out").identity;
    body.status = StepStatus::Failed;
    body.exit_code = Some(1);
    let failed = Receipt::seal(body, Observed::default());
    store.put_bytes(b"out").unwrap();
    store.put_bytes(b"").unwrap();
    store.put_receipt(&failed).unwrap();
    store.remember(&key, &failed.receipt_id, &[]).unwrap();
    assert!(store.recall(&key).unwrap().is_none());
}

#[test]
fn cache_misses_when_the_receipt_was_tampered_with() {
    let (_dir, store) = store();
    let key = digest::bytes(b"step-key");
    let mut receipt = receipt(&key, b"out");
    store.put_bytes(b"out").unwrap();
    store.put_bytes(b"").unwrap();
    store.put_receipt(&receipt).unwrap();
    store.remember(&key, &receipt.receipt_id, &[]).unwrap();
    assert!(store.recall(&key).unwrap().is_some());

    // Another body under the same id: it still loads, but is not that receipt.
    receipt.identity.command = "echo forged".into();
    fs::write(
        store.receipt_path(&receipt.receipt_id).unwrap(),
        serde_json::to_string_pretty(&receipt).unwrap(),
    )
    .unwrap();
    assert!(store.load_receipt(&receipt.receipt_id).is_ok());
    assert!(store.recall(&key).unwrap().is_none());
}

#[test]
fn cache_misses_when_the_entry_names_another_steps_receipt() {
    let (_dir, store) = store();
    let key = digest::bytes(b"step-key");
    let other = receipt(&digest::bytes(b"other-step-key"), b"out");
    store.put_bytes(b"out").unwrap();
    store.put_bytes(b"").unwrap();
    store.put_receipt(&other).unwrap();
    store.remember(&key, &other.receipt_id, &[]).unwrap();
    assert!(store.recall(&key).unwrap().is_none());
}

#[test]
fn cache_misses_on_an_unknown_key() {
    let (_dir, store) = store();
    assert!(store
        .recall(&digest::bytes(b"never-seen"))
        .unwrap()
        .is_none());
}

/// The exact bytes of a sealed cache entry: its fields in order and the
/// seal this host's secret puts on them. These change only with the
/// store format, or every cache entry a host wrote stops being believed.
#[test]
fn a_sealed_cache_entry_has_exactly_these_bytes() {
    let (_dir, store) = store();
    let entry = |step_key: &[u8], vouched_by: &[String]| {
        let step_key = digest::bytes(step_key);
        store
            .remember(&step_key, &digest::bytes(b"receipt"), vouched_by)
            .unwrap();
        fs::read_to_string(store.cache_path(&step_key).unwrap()).unwrap()
    };
    assert_eq!(entry(b"ran here", &[]), GOLDEN_LOCAL_ENTRY);
    assert_eq!(
        entry(b"imported", &["ed25519:00".into()]),
        GOLDEN_IMPORTED_ENTRY
    );
}

const GOLDEN_LOCAL_ENTRY: &str = r#"{
  "step_key": "sha256:12ae9ef07cb551a948d684543b148f8c3b77f08c675ef8bafe37db723d8d784d",
  "receipt_id": "sha256:6f32860910ca0fb2a20c7fda143666b09dbf8db5238195c90a586fb542ff0cad",
  "vouched_by": [],
  "seal": "7c5ebe018a021e44bc43feff6d3063313e4774b850095046a7075400b2496518"
}"#;

const GOLDEN_IMPORTED_ENTRY: &str = r#"{
  "step_key": "sha256:5f54227b74fbba7743c47cd286b4873f2e17331518d56facfc03e34cde4a0950",
  "receipt_id": "sha256:6f32860910ca0fb2a20c7fda143666b09dbf8db5238195c90a586fb542ff0cad",
  "vouched_by": [
    "ed25519:00"
  ],
  "seal": "87b4571d2a397671d0f66d987f462a16d568236ad9d10ce756689d0328a7133c"
}"#;
