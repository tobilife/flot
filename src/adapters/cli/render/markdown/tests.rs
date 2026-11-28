//! Tests of the Markdown report's text.

use super::md;

/// Control characters other than line breaks, which a claim from an agent
/// may carry, are dropped: a tab, a bell, DEL, an escape and a C1 control
/// reach the report as nothing.
#[test]
fn control_characters_are_dropped_from_markdown_text() {
    assert_eq!(md("tab\there\u{7}\u{7f}\u{9b}\u{1b}[0m"), "tabhere\\[0m");
    assert_eq!(md("two\nlines\r"), "two lines ");
}
