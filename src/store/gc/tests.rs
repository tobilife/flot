//! Tests of gc: what it removes, what it keeps, a dry run, and that it never
//! runs under a writer.

use std::fs;

use super::super::tests::{receipt, run, store, test_host};
use super::super::Store;
use crate::core::digest;

/// A run id no recorded run has.
const GONE: &str = "1795496079152-c5fe4934ed4a";

#[test]
fn gc_removes_evidence_that_no_run_references() {
    let (_dir, store) = store();
    let orphan = store.put_bytes(b"orphan").unwrap();
    let receipt = receipt(&digest::bytes(b"k"), b"orphan");
    store.put_receipt(&receipt).unwrap();
    store
        .remember(&digest::bytes(b"k"), &receipt.receipt_id, &[])
        .unwrap();
    store.add_attestations(GONE, &[]).unwrap();

    let build = store.root().join("builds/s/debug");
    fs::create_dir_all(&build).unwrap();
    fs::write(build.join("tool"), b"built").unwrap();

    let report = store.gc(false).unwrap();
    assert!(!store.root().join("builds").exists());
    assert_eq!(report.removed_receipts, 1);
    assert_eq!(report.removed_objects, 1);
    assert_eq!(report.removed_cache, 1);
    assert!(!store.has_object(&orphan));
    assert!(store.load_receipt(&receipt.receipt_id).is_err());
    assert!(store.entries("cache", ".json").unwrap().is_empty());
    assert!(store.recall(&digest::bytes(b"k")).unwrap().is_none());
    // What only describes a run goes with it.
    assert!(!store
        .root()
        .join(format!("attestations/{GONE}.json"))
        .exists());
}

#[test]
fn gc_keeps_everything_a_recorded_run_references() {
    let (_dir, store) = store();
    let kept = receipt(&digest::bytes(b"k"), b"out");
    store.put_bytes(b"out").unwrap();
    store.put_bytes(b"").unwrap();
    let observed = store.put_bytes(b"observation").unwrap();
    store.put_receipt(&kept).unwrap();
    let run = run(1_795_496_079_152, &kept, &observed);
    store.save_imported_run(&run).unwrap();
    store.add_attestations(&run.run_id, &[]).unwrap();
    let orphan = store.put_bytes(b"orphan").unwrap();

    let report = store.gc(false).unwrap();
    assert_eq!(report.kept_receipts, 1);
    assert_eq!(report.removed_receipts, 0);
    // Its output streams and this run's observation of it.
    assert_eq!(report.kept_objects, 3);
    assert_eq!(report.removed_objects, 1);
    assert!(!store.has_object(&orphan));
    assert_eq!(store.resolve_run(&run.run_id).unwrap(), run);
    assert!(store.load_receipt(&kept.receipt_id).is_ok());
    assert!(store.has_object(&observed));
    // What describes the run stays with it.
    assert!(store.is_imported(&run.run_id));
    assert!(store
        .root()
        .join(format!("attestations/{}.json", run.run_id))
        .exists());
}

#[test]
fn gc_dry_run_reports_without_deleting() {
    let (_dir, store) = store();
    let orphan = store.put_bytes(b"orphan").unwrap();
    let receipt = receipt(&digest::bytes(b"k"), b"orphan");
    store.put_receipt(&receipt).unwrap();
    store
        .remember(&digest::bytes(b"k"), &receipt.receipt_id, &[])
        .unwrap();
    store.add_attestations(GONE, &[]).unwrap();
    fs::create_dir_all(store.root().join("builds/s")).unwrap();
    let report = store.gc(true).unwrap();
    assert!(store.root().join("builds/s").exists());
    assert!(report.dry_run);
    assert_eq!(report.removed_objects, 1);
    assert_eq!(report.removed_receipts, 1);
    assert_eq!(report.removed_cache, 1);
    assert!(store.has_object(&orphan), "a dry run must not delete");
    assert!(store.load_receipt(&receipt.receipt_id).is_ok());
    assert_eq!(store.entries("cache", ".json").unwrap().len(), 1);
    assert!(store
        .root()
        .join(format!("attestations/{GONE}.json"))
        .exists());
}

#[test]
fn gc_of_a_store_not_written_yet_reports_what_was_asked_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), test_host()).unwrap();
    assert!(store.gc(true).unwrap().dry_run);
    assert!(!store.gc(false).unwrap().dry_run);
    assert!(!store.root().exists());
}

#[test]
fn gc_waits_for_no_one_and_never_runs_under_a_writer() {
    let (_dir, store) = store();
    let _writing = store.lock(false).unwrap();
    let error = store.gc(false).unwrap_err().to_string();
    assert!(error.contains("in use by a running flot"), "{error}");
}
