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

/// A receipt altered after it was recorded is not searched: what it says
/// cannot be believed, and the report names it instead.
#[test]
fn an_altered_receipt_is_named_unreadable_not_searched() {
    use crate::service::{RunOptions, Workspace};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("flot.toml"),
        "[flow]\nname = \"t\"\n\n[[step]]\nid = \"a\"\nrun = \"echo needle\"\n",
    )
    .unwrap();
    let workspace = Workspace::at(dir.path(), &dir.path().join("flot.toml"));
    let run = workspace.run(&RunOptions::default()).unwrap();
    assert_eq!(workspace.search("needle", 50, 50).unwrap().hits.len(), 1);

    let receipt_id = &run.manifest.steps[0].receipt_id;
    let path = std::fs::read_dir(dir.path().join(".flot/receipts"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| receipt_id.ends_with(path.file_stem().unwrap().to_str().unwrap()))
        .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("echo needle", "echo altered")).unwrap();
    let report = workspace.search("needle", 50, 50).unwrap();
    assert!(report.hits.is_empty(), "{:?}", report.hits);
    assert_eq!(report.unreadable.len(), 1, "{:?}", report.unreadable);
    assert!(
        report.unreadable[0].contains("does not match its body"),
        "{:?}",
        report.unreadable
    );
}
