//! Tests of how a hit's line is cut around its match.

use super::*;

#[test]
fn a_long_line_is_cut_around_the_match() {
    let line = format!("{}NEEDLE{}", "a".repeat(500), "b".repeat(500));
    let cut = around(&line, "needle");
    assert!(cut.contains("NEEDLE"), "{cut}");
    assert!(cut.starts_with('…') && cut.ends_with('…'), "{cut}");
    assert_eq!(cut.chars().count(), LINE_CHARS + 2);
    assert_eq!(around("short NEEDLE", "needle"), "short NEEDLE");
    assert_eq!(around("two\nlines", "two"), "two lines");
}
