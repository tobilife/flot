//! Tests of content-addressed objects: their round trip, and what a corrupt
//! one does.

use std::fs;

use super::super::tests::store;
use crate::core::digest;

#[test]
fn objects_round_trip_under_their_digest() {
    let (_dir, store) = store();
    let object = store.put_bytes(b"hello").unwrap();
    assert_eq!(object, digest::bytes(b"hello"));
    assert_eq!(store.read_bytes(&object).unwrap(), b"hello");
    assert!(store.has_object(&object));
}

#[test]
fn storing_the_same_bytes_twice_is_idempotent() {
    let (_dir, store) = store();
    let first = store.put_bytes(b"same").unwrap();
    let second = store.put_bytes(b"same").unwrap();
    assert_eq!(first, second);
}

#[test]
fn a_corrupted_object_file_is_reported_not_returned() {
    let (_dir, store) = store();
    let object = store.put_bytes(b"trusted").unwrap();
    fs::write(store.object_path(&object).unwrap(), b"swapped").unwrap();
    let err = store.read_bytes(&object).unwrap_err();
    assert!(
        err.to_string().contains("does not match its content"),
        "{err}"
    );
}

#[test]
fn unknown_digest_forms_are_rejected() {
    let (_dir, store) = store();
    assert!(store.read_bytes("md5:abc").is_err());
    assert!(store.read_bytes("sha256:short").is_err());
}

#[test]
fn a_corrupt_object_is_replaced_when_written_again() {
    let (dir, store) = store();
    let object = store.put_bytes(b"real").unwrap();
    let hex = &object["sha256:".len()..];
    let path = dir.path().join(".flot/objects").join(&hex[..2]).join(hex);
    std::fs::write(&path, b"rot").unwrap();
    assert!(store.read_bytes(&object).is_err());
    store.put_bytes(b"real").unwrap();
    assert_eq!(store.read_bytes(&object).unwrap(), b"real");
}
