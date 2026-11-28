//! Tests of reading a hook's payload and a tool's name.

use super::tool::{reads_only, words_of};
use super::{repair_surrogates, unpanicked};

#[test]
fn a_lone_surrogate_escape_is_replaced_and_a_pair_is_kept() {
    assert_eq!(
        repair_surrogates(r#"{"a":"x\ud83d"}"#),
        r#"{"a":"x\ufffd"}"#
    );
    assert_eq!(
        repair_surrogates(r#""\ude00 \ud83d\ude00""#),
        r#""\ufffd \ud83d\ude00""#
    );
    assert_eq!(repair_surrogates(r#""\\ud83d""#), r#""\\ud83d""#);
    assert_eq!(repair_surrogates(r#""\ud83d"#), r#""\ufffd"#);
    let value: serde_json::Value =
        serde_json::from_str(&repair_surrogates(r#"{"c":"rm check.sh \ud83d"}"#)).unwrap();
    assert_eq!(value["c"], "rm check.sh \u{fffd}");
}

#[test]
fn a_panic_in_the_guard_is_an_error() {
    let failed = unpanicked(|| -> anyhow::Result<()> { panic!("boom") });
    assert!(format!("{:#}", failed.unwrap_err()).contains("boom"));
    assert_eq!(unpanicked(|| Ok(1)).unwrap(), 1);
}

#[test]
fn a_tool_is_read_only_by_its_name_unless_it_names_a_change() {
    assert_eq!(
        words_of("mcp__fs__read_text_file"),
        ["mcp", "fs", "read", "text", "file"]
    );
    assert_eq!(words_of("NotebookEdit"), ["notebook", "edit"]);
    for tool in [
        "Read",
        "LS",
        "Glob",
        "Grep",
        "WebFetch",
        "mcp__fs__list_directory",
    ] {
        assert!(reads_only(tool), "{tool}");
    }
    for tool in [
        "Write",
        "mcp__fs__write_file",
        "mcp__fs__move_file",
        "search_replace",
        "readAndWrite",
        "download_file",
        "TodoWrite",
    ] {
        assert!(!reads_only(tool), "{tool}");
    }
}
