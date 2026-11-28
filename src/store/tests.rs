//! Tests of opening a store and of the host's seal, and the helpers every
//! store test shares.

use std::collections::BTreeMap;

use super::{HostSecret, Store, STORE_DIR};
use crate::core::digest;
use crate::core::receipt::{Observed, Receipt, ReceiptIdentity, StepStatus, ToolIdentity};

fn test_host() -> HostSecret {
    HostSecret::from_bytes([7; 32])
}

pub(super) fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), test_host()).unwrap();
    store.ensure().unwrap();
    (dir, store)
}

#[test]
fn a_store_in_another_format_is_refused_not_misread() {
    let (dir, store) = store();
    drop(store);
    assert!(
        Store::open(dir.path(), test_host()).is_ok(),
        "reopening is fine"
    );
    std::fs::write(dir.path().join(".flot/FORMAT"), "1").unwrap();
    let error = Store::open(dir.path(), test_host())
        .unwrap_err()
        .to_string();
    assert!(error.contains("store format 1"), "{error}");

    let old = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(old.path().join(".flot/runs")).unwrap();
    std::fs::write(old.path().join(".flot/runs/1-a.json"), "{}").unwrap();
    let error = Store::open(old.path(), test_host())
        .unwrap_err()
        .to_string();
    assert!(error.contains("older flot"), "{error}");
}

pub(super) fn receipt(step_key: &str, stdout: &[u8]) -> Receipt {
    Receipt::seal(
        ReceiptIdentity {
            flow: "demo".into(),
            step_id: "a".into(),
            step_key: step_key.into(),
            command: "echo hi".into(),
            status: StepStatus::Ok,
            exit_code: Some(0),
            stdout_digest: digest::bytes(stdout),
            stderr_digest: digest::bytes(b""),
            inputs: vec![],
            verify_inputs: vec![],
            outputs: vec![],
            changed: Default::default(),
            deps: vec![],
            claims: vec![],
            limitations: vec![],
            tool: ToolIdentity::default(),
            candidate: None,
            sandbox: None,
        },
        Observed::default(),
    )
}

#[test]
fn a_cache_entry_or_trust_list_from_another_host_is_not_believed() {
    let (dir, store) = store();
    let key = digest::bytes(b"step-key");
    let receipt = receipt(&key, b"out");
    store.put_bytes(b"out").unwrap();
    store.put_bytes(b"").unwrap();
    store.put_receipt(&receipt).unwrap();
    store.remember(&key, &receipt.receipt_id, &[]).unwrap();
    store
        .save_trusted_keys(&BTreeMap::from([("ed25519:00".into(), "alice".into())]))
        .unwrap();

    // The same files, read by a host with another secret.
    let elsewhere = Store::open(dir.path(), HostSecret::from_bytes([8; 32])).unwrap();
    assert!(elsewhere.recall(&key).unwrap().is_none());
    let error = elsewhere.trusted_keys().unwrap_err().to_string();
    assert!(error.contains("not written by this host"), "{error}");
    assert!(store.recall(&key).unwrap().is_some());
    assert_eq!(store.trusted_keys().unwrap().len(), 1);
}

#[test]
fn nothing_is_created_until_something_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), test_host()).unwrap();
    assert!(store.list_runs().unwrap().is_empty());
    assert!(store.recall(&digest::bytes(b"k")).unwrap().is_none());
    assert!(!dir.path().join(STORE_DIR).exists());
}
