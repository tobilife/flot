//! Tests of reading a hook's payload, a tool's name, and a tool's input.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::tool::{reads_only, words_of};
use super::{repair_surrogates, tool_call, unpanicked};

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

/// The paths a tool's `input` would change, read from `/work`.
fn targets(tool: &str, input: &Value) -> Vec<PathBuf> {
    match tool_call(Path::new("/work"), tool, input) {
        Ok(call) => call.targets,
        Err(error) => panic!("{error:#}"),
    }
}

#[test]
fn a_notebook_edit_changes_its_notebook_path_and_cannot_be_judged_without_one() {
    assert_eq!(
        targets("NotebookEdit", &json!({ "notebook_path": "a.ipynb" })),
        [PathBuf::from("/work/a.ipynb")]
    );
    let Err(error) = tool_call(
        Path::new("/work"),
        "NotebookEdit",
        &json!({ "file_path": "a.ipynb" }),
    ) else {
        panic!("a NotebookEdit without a notebook_path is judged");
    };
    assert_eq!(
        format!("{error:#}"),
        "the NotebookEdit call names no notebook_path"
    );
}

#[test]
fn an_empty_path_in_another_tools_input_names_nothing() {
    assert_eq!(
        targets(
            "mcp__fs__write_file",
            &json!({ "path": "", "paths": ["", "b.txt"] })
        ),
        [PathBuf::from("/work/b.txt")]
    );
}

#[test]
fn another_tools_input_is_read_for_paths_32_levels_deep_and_no_deeper() {
    let named = json!({ "path": "deep.txt" });
    let under_objects =
        |levels: usize| (0..levels).fold(named.clone(), |inner, _| json!({ "a": inner }));
    let under_arrays = |levels: usize| (0..levels).fold(named.clone(), |inner, _| json!([inner]));
    let (deep, none) = (vec![PathBuf::from("/work/deep.txt")], Vec::<PathBuf>::new());
    assert_eq!(targets("mcp__fs__write_file", &under_objects(32)), deep);
    assert_eq!(targets("mcp__fs__write_file", &under_objects(33)), none);
    assert_eq!(targets("mcp__fs__write_file", &under_arrays(32)), deep);
    assert_eq!(targets("mcp__fs__write_file", &under_arrays(33)), none);
}
