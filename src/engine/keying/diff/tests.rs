//! Tests of what changed between two digests.

use super::super::tests::input;
use super::*;

#[test]
fn changed_paths_reports_edits_creations_and_deletions() {
    let before = [
        input("kept", b"1"),
        input("edited", b"1"),
        input("gone", b"1"),
    ];
    let after = [
        input("kept", b"1"),
        input("edited", b"2"),
        input("new", b"1"),
    ];
    assert_eq!(
        changed_paths(&before, &after),
        vec!["edited", "gone", "new"]
    );
    assert!(changed_paths(&before, &before).is_empty());
}
