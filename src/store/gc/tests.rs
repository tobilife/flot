//! Tests of gc: what it removes, a dry run, and that it never runs under a
//! writer.

use std::fs;

use super::super::tests::{receipt, store};
use crate::core::digest;

#[test]
fn gc_removes_evidence_that_no_run_references() {
    let (_dir, store) = store();
    let orphan = store.put_bytes(b"orphan").unwrap();
    let receipt = receipt(&digest::bytes(b"k"), b"orphan");
    store.put_receipt(&receipt).unwrap();
    store
        .remember(&digest::bytes(b"k"), &receipt.receipt_id, &[])
        .unwrap();

    let build = store.root().join("builds/s/debug");
    fs::create_dir_all(&build).unwrap();
    fs::write(build.join("tool"), b"built").unwrap();

    let report = store.gc(false).unwrap();
    assert!(!store.root().join("builds").exists());
    assert_eq!(report.removed_receipts, 1);
    assert_eq!(report.removed_objects, 1);
    assert_eq!(report.removed_cache, 1);
    assert!(!store.has_object(&orphan));
    assert!(store.recall(&digest::bytes(b"k")).unwrap().is_none());
}

#[test]
fn gc_dry_run_reports_without_deleting() {
    let (_dir, store) = store();
    let orphan = store.put_bytes(b"orphan").unwrap();
    fs::create_dir_all(store.root().join("builds/s")).unwrap();
    let report = store.gc(true).unwrap();
    assert!(store.root().join("builds/s").exists());
    assert!(report.dry_run);
    assert_eq!(report.removed_objects, 1);
    assert!(store.has_object(&orphan), "a dry run must not delete");
}

#[test]
fn gc_waits_for_no_one_and_never_runs_under_a_writer() {
    let (_dir, store) = store();
    let _writing = store.lock(false).unwrap();
    let error = store.gc(false).unwrap_err().to_string();
    assert!(error.contains("in use by a running flot"), "{error}");
}
