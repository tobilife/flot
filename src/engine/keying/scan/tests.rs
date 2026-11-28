//! Tests of scans.

use super::*;
use crate::core::digest;

/// Over this repository's own files, enough for several threads, the
/// files read at once get the digests, bits and order that reading them
/// one at a time gives. The bits are the file system's, as a snapshot
/// reads them, not the ones git records.
#[test]
fn reading_files_at_once_records_what_reading_them_in_turn_does() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let patterns = ["src/**/*.rs".to_string(), "tests/**/*".to_string()];
    let scan = |keep| scan_in(root, None, &patterns, "input", Reach::Tracked, false, keep);
    let digests = Digests::default();
    let at_once = scan(Keep::<std::convert::Infallible>::Digest(&digests)).unwrap();
    let mut one = |bytes: &[u8]| Ok(digest::bytes(bytes));
    let in_turn = scan(Keep::With(&mut one)).unwrap();
    assert!(at_once.files.len() >= 64, "{} files", at_once.files.len());
    assert_eq!(at_once.files, in_turn.files);
    assert!(at_once.problems.is_empty() && at_once.unmatched.is_empty());
}
