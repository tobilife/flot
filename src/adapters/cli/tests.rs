//! Tests of what the command line prints on a terminal.

use super::terminal_safe;

#[test]
fn control_sequences_and_bidi_overrides_are_made_visible() {
    assert_eq!(terminal_safe("plain\ttext\nok"), "plain\ttext\nok");
    assert_eq!(terminal_safe("\u{1b}[2Jgone"), "\\u{1b}[2Jgone");
    assert_eq!(terminal_safe("over\rwrite"), "over\\u{d}write");
    assert_eq!(terminal_safe("a\u{202E}b"), "a\\u{202e}b");
    assert_eq!(terminal_safe("한글 ✓"), "한글 ✓");
}
