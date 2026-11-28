//! Tests of run ids and run selectors: none can name a file outside the store,
//! and a short prefix is enough to name a run.

use super::super::tests::{receipt, run, store};
use super::{is_run_id, MIN_RUN_PREFIX};
use crate::core::digest;

#[test]
fn a_run_id_can_never_name_a_file_outside_the_store() {
    let (_dir, store) = store();
    for id in ["../escape", "a/b", "", "..", "x.json"] {
        assert!(store.attestations(id).is_err(), "{id}");
        assert!(store.add_attestations(id, &[]).is_err(), "{id}");
    }
    assert!(store.attestations("1795496079152-c5fe4934ed4a").is_ok());
}

#[test]
fn a_run_id_is_a_13_digit_stamp_and_12_hex_digits_of_its_root() {
    assert!(is_run_id("1795496079152-c5fe4934ed4a"));
    for id in [
        "179549607915-c5fe4934ed4a",
        "179549607915x-c5fe4934ed4a",
        "1795496079152-c5fe4934ed4",
        "1795496079152-c5fe4934ed4z",
    ] {
        assert!(!is_run_id(id), "{id}");
    }
}

#[test]
fn a_run_is_named_by_the_shortest_prefix_of_its_id_or_root_allowed() {
    let (_dir, store) = store();
    let receipt = receipt(&digest::bytes(b"k"), b"out");
    let run = run(1_795_496_079_152, &receipt, &digest::bytes(b"observation"));
    store.save_run(&run).unwrap();
    let (_, root) = run.run_id.split_once('-').unwrap();
    for spec in [&run.run_id[..MIN_RUN_PREFIX], &root[..MIN_RUN_PREFIX]] {
        assert_eq!(store.resolve_run(spec).unwrap(), run, "{spec}");
    }
    let error = store
        .resolve_run(&root[..MIN_RUN_PREFIX - 1])
        .unwrap_err()
        .to_string();
    assert!(error.contains("too short to name a run"), "{error}");
}
