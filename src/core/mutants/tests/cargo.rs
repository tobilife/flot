//! Tests of reading cargo-mutants' outcomes: a survivor named without its
//! place, and the outcomes refused as incomplete or not understood.

use anyhow::Result;

use crate::core::digest;
use crate::core::mutants::{read_report, Report, Survivor};

/// cargo-mutants 27.1.0 on flot: the baseline, a mutant it missed in
/// `Builds::digests` and one it caught, as `outcomes.json` and
/// `mutants.json` record them.
fn cargo(baseline: &str, missed: &str) -> (String, String) {
    let mutant = |name: &str, line: u32, start: u32, end: u32| {
        serde_json::json!({
            "name": name, "package": "flot-cli", "file": "src/store.rs",
            "span": { "start": { "line": line, "column": start }, "end": { "line": line, "column": end } },
            "replacement": "Default::default()", "genre": "FnValue"
        })
    };
    let digests = mutant(
        "src/store.rs:3:9: replace Builds::digests -> PathBuf with Default::default()",
        3,
        9,
        36,
    );
    let work = mutant(
        "src/store.rs:1:9: replace Builds::work -> PathBuf with Default::default()",
        1,
        9,
        31,
    );
    let outcomes = serde_json::json!({
        "outcomes": [
            { "scenario": "Baseline", "summary": baseline },
            { "scenario": { "Mutant": digests }, "summary": missed },
            { "scenario": { "Mutant": work }, "summary": "CaughtMutant" },
        ],
        "total_mutants": 2, "missed": 1, "caught": 1, "timeout": 0, "unviable": 0,
        "success": 0, "cargo_mutants_version": "27.1.0"
    });
    (
        outcomes.to_string(),
        serde_json::json!([digests, work]).to_string(),
    )
}

const STORE: &str =
    "        self.root.join(\"work\")\n    /// What the copy's files held.\n        self.root.join(\"work.json\")\n";

fn read_cargo(outcomes: &str, planned: Option<&str>) -> Result<Report> {
    read_report(
        outcomes.as_bytes(),
        planned.map(str::as_bytes),
        &|path| (path == "src/store.rs").then(|| STORE.to_string()),
        std::path::Path::new("/work"),
    )
}

#[test]
fn cargo_mutants_outcomes_name_a_survivor_by_its_description_without_its_place() {
    let (outcomes, planned) = cargo("Success", "MissedMutant");
    let report = read_cargo(&outcomes, Some(&planned)).unwrap();
    assert_eq!((report.caught, report.untested), (1, 0));
    assert_eq!(
        report.survived,
        [Survivor {
            file: "src/store.rs".into(),
            mutation: "replace Builds::digests -> PathBuf with Default::default()".into(),
            code: digest::bytes(b"self.root.join(\"work.json\")"),
            line: 3,
        }]
    );
    let (outcomes, planned) = cargo("Success", "Timeout");
    assert_eq!(read_cargo(&outcomes, Some(&planned)).unwrap().caught, 2);
    let (outcomes, planned) = cargo("Success", "Unviable");
    assert_eq!(read_cargo(&outcomes, Some(&planned)).unwrap().untested, 1);
}

#[test]
fn cargo_mutants_outcomes_that_are_not_all_there_are_refused() {
    let (outcomes, planned) = cargo("Failure", "MissedMutant");
    let error = format!("{:#}", read_cargo(&outcomes, Some(&planned)).unwrap_err());
    assert!(error.contains("failed before any mutation"), "{error}");

    let (outcomes, planned) = cargo("Success", "MissedMutant");
    let error = format!("{:#}", read_cargo(&outcomes, None).unwrap_err());
    assert!(error.contains("no mutants.json beside it"), "{error}");

    // Stopped before the last mutant: only the plan tells.
    let value: serde_json::Value = serde_json::from_str(&outcomes).unwrap();
    let mut stopped = value.clone();
    stopped["outcomes"].as_array_mut().unwrap().pop();
    let error = format!(
        "{:#}",
        read_cargo(&stopped.to_string(), Some(&planned)).unwrap_err()
    );
    assert!(
        error.contains("replace Builds::work -> PathBuf with Default::default() was never tested"),
        "{error}"
    );
    stopped["outcomes"].as_array_mut().unwrap().pop();
    let error = format!(
        "{:#}",
        read_cargo(&stopped.to_string(), Some(&planned)).unwrap_err()
    );
    assert!(error.contains(", nor 1 more"), "{error}");

    let (outcomes, planned) = cargo("Success", "Exploded");
    let error = format!("{:#}", read_cargo(&outcomes, Some(&planned)).unwrap_err());
    assert!(error.contains("unknown outcome `Exploded`"), "{error}");

    let (outcomes, planned) = cargo("Success", "MissedMutant");
    let error = format!(
        "{:#}",
        read_report(
            outcomes.as_bytes(),
            Some(planned.as_bytes()),
            &|_| None,
            std::path::Path::new("/work")
        )
        .unwrap_err()
    );
    assert!(error.contains("cannot read src/store.rs"), "{error}");
}
