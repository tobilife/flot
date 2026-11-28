//! Tests of reading a mutation testing report: what survived and what it
//! changed, and the reports refused as unfinished or not understood.

use anyhow::Result;

use crate::core::digest;
use crate::core::mutants::{read_report, Report, Survivor};

const SOURCE: &str = "export class SidecarContractError extends Error {\n  constructor(message: string) {\n    super(message);\n    this.name = \"SidecarContractError\";\n  }\n}\n\nfunction isRecord(value: unknown): value is Record<string, unknown> {\n  if (typeof value !== \"object\" || value == null || Array.isArray(value)) {\n    return false;\n  }\n  return true;\n}\n";

/// A StrykerJS 10.1.1 report on open-design's sidecar-proto, its source
/// cut to two functions and the mutants placed in it.
fn stryker(statuses: [&str; 4]) -> String {
    serde_json::json!({
        "schemaVersion": "1.0",
        "thresholds": { "high": 80, "low": 60, "break": null },
        "projectRoot": "/work/packages/sidecar-proto",
        "files": { "src/index.ts": { "language": "typescript", "source": SOURCE, "mutants": [
            { "id": "1", "mutatorName": "StringLiteral", "replacement": "\"\"", "status": statuses[0],
              "location": { "start": { "line": 4, "column": 17 }, "end": { "line": 4, "column": 39 } } },
            { "id": "4", "mutatorName": "ConditionalExpression", "replacement": "false", "status": statuses[1],
              "location": { "start": { "line": 9, "column": 7 }, "end": { "line": 9, "column": 73 } } },
            { "id": "6", "mutatorName": "ConditionalExpression", "replacement": "false", "status": statuses[2],
              "location": { "start": { "line": 9, "column": 7 }, "end": { "line": 9, "column": 49 } } },
            { "id": "0", "mutatorName": "BlockStatement", "replacement": "{}", "status": statuses[3],
              "location": { "start": { "line": 2, "column": 32 }, "end": { "line": 5, "column": 4 } } }
        ] } }
    })
    .to_string()
}

fn read(report: &str) -> Result<Report> {
    read_report(
        report.as_bytes(),
        None,
        &|_| None,
        std::path::Path::new("/work"),
    )
}

#[test]
fn a_schema_report_counts_what_survived_by_the_code_it_changes() {
    let report = read(&stryker(["Survived", "Killed", "NoCoverage", "Timeout"])).unwrap();
    assert_eq!((report.caught, report.untested), (2, 0));
    let code = |text: &str| digest::bytes(text.as_bytes());
    assert_eq!(
        report.survived,
        [
            Survivor {
                file: "packages/sidecar-proto/src/index.ts".into(),
                mutation: "ConditionalExpression: false".into(),
                code: code("typeof value !== \"object\" || value == null"),
                line: 9,
            },
            Survivor {
                file: "packages/sidecar-proto/src/index.ts".into(),
                mutation: "StringLiteral: \"\"".into(),
                code: code("\"SidecarContractError\""),
                line: 4,
            },
        ]
    );
    // A span over lines keeps them whole between its ends.
    let block = read(&stryker(["Killed", "Killed", "Killed", "Survived"])).unwrap();
    assert_eq!(
        block.survived[0].code,
        code("{\n    super(message);\n    this.name = \"SidecarContractError\";\n  }")
    );
    let untested = read(&stryker([
        "CompileError",
        "RuntimeError",
        "Ignored",
        "Killed",
    ]));
    assert_eq!(untested.unwrap().untested, 3);
    // A project root may be given relative to the flow root.
    let relative = read(
        &stryker(["Survived", "Killed", "Killed", "Killed"])
            .replace("/work/packages/sidecar-proto", "packages/proto"),
    )
    .unwrap();
    assert_eq!(relative.survived[0].file, "packages/proto/src/index.ts");
    // The flow root itself, and a file named from the root.
    let at_root = read(
        &stryker(["Survived", "Killed", "Killed", "Killed"])
            .replace("/work/packages/sidecar-proto", "/work")
            .replace("\"src/index.ts\"", "\"/work/lib/index.ts\""),
    )
    .unwrap();
    assert_eq!(at_root.survived[0].file, "lib/index.ts");
}

#[test]
fn a_schema_report_that_is_not_done_or_not_understood_is_refused() {
    for (report, refusal) in [
        (
            stryker(["Killed", "Pending", "Killed", "Killed"]),
            "mutant 4 of src/index.ts was never tested",
        ),
        (
            stryker(["Killed", "Unknown", "Killed", "Killed"]),
            "unknown status `Unknown`",
        ),
        (
            stryker(["Killed"; 4]).replace("\"1.0\"", "\"3\""),
            "schema version 3",
        ),
        (
            stryker(["Killed"; 4]).replace("/work/packages", "/elsewhere"),
            "not inside the flow root",
        ),
        (
            stryker(["Killed"; 4]).replace("/work/packages/sidecar-proto", "/work/../x"),
            "not inside the flow root",
        ),
        (
            stryker(["Killed"; 4]).replace("/work/packages/sidecar-proto", "../x"),
            "not inside the flow root",
        ),
        (
            stryker(["Killed"; 4]).replace("\"src/index.ts\"", "\"/etc/index.ts\""),
            "the report names /etc/index.ts, outside the flow root",
        ),
        (
            stryker(["Survived", "Killed", "Killed", "Killed"])
                .replace("\"line\":4,\"column\":39", "\"line\":4,\"column\":99"),
            "placed outside its source",
        ),
        ("{}".to_string(), "neither a mutation testing report"),
        ("[".to_string(), "not JSON"),
    ] {
        let error = format!("{:#}", read(&report).unwrap_err());
        assert!(error.contains(refusal), "{refusal}: {error}");
    }
}
