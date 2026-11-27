//! What a mutation tester reports of a proof: the mutants it made of the
//! code, and which of them the proof's tests caught. A mutant that survived
//! is a change to the code the proof cannot tell from the code itself.
//!
//! Two reports are read: the mutation testing report schema (Stryker and
//! the tools that share it) and cargo-mutants' `outcomes.json`, with the
//! `mutants.json` beside it that says which mutants were to be tested. Both
//! place a mutant by 1-based line and column, its end excluded.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::digest;
use crate::core::flow::is_contained;

/// A mutant the proof did not catch, as a person names it in the allow list:
/// the file, what the mutation does, and the code it changes, so the entry
/// still names it when other code moves it but no longer once its code
/// changes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Survivor {
    /// The mutated file, relative to the flow root.
    pub file: String,
    /// What the mutation does: cargo-mutants' description, which names the
    /// function, or the schema's mutator and its replacement.
    pub mutation: String,
    /// Digest of the code the mutation replaces.
    pub code: String,
    /// Where it was, for a reader; not part of what an entry matches.
    pub line: u32,
}

/// What a complete report says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Mutants the proof's tests caught, timeouts included.
    pub caught: u32,
    /// Mutants that could not be tested: they did not build, or the tool
    /// was told to ignore them.
    pub untested: u32,
    pub survived: Vec<Survivor>,
}

/// Read a report. `planned` is the `mutants.json` cargo-mutants wrote beside
/// its outcomes, when there is one; `source` reads a file the report names,
/// relative to the flow root, and is asked only of cargo-mutants' reports,
/// which carry no source. `root` is the flow root, which a schema report's
/// project root names the package of its paths within.
pub fn read_report(
    report: &[u8],
    planned: Option<&[u8]>,
    source: &dyn Fn(&str) -> Option<String>,
    root: &std::path::Path,
) -> Result<Report> {
    let value: serde_json::Value =
        serde_json::from_slice(report).context("the report is not JSON")?;
    if value.get("cargo_mutants_version").is_some() {
        let planned = planned
            .context("cargo-mutants' outcomes.json has no mutants.json beside it, which lists the mutants to test")?;
        cargo_mutants(value, planned, source)
    } else if value.get("schemaVersion").is_some() {
        schema(value, root)
    } else {
        bail!(
            "the report is neither a mutation testing report (no `schemaVersion`) nor cargo-mutants' outcomes.json"
        )
    }
}

/// The text from `start` to `end`, 1-based lines and columns, the end
/// excluded; columns count characters.
fn span(text: &str, start: (u32, u32), end: (u32, u32)) -> Option<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let line = |number: u32| lines.get(usize::try_from(number).ok()?.checked_sub(1)?);
    let from = |line: &str, column: u32| -> Option<usize> {
        let skip = usize::try_from(column).ok()?.checked_sub(1)?;
        if skip == line.chars().count() {
            return Some(line.len());
        }
        line.char_indices().nth(skip).map(|(at, _)| at)
    };
    let (first, last) = (line(start.0)?, line(end.0)?);
    match start.0.cmp(&end.0) {
        std::cmp::Ordering::Greater => None,
        std::cmp::Ordering::Equal => {
            let (a, b) = (from(first, start.1)?, from(first, end.1)?);
            first.get(a..b).map(str::to_string)
        }
        std::cmp::Ordering::Less => {
            let mut code = first.get(from(first, start.1)?..)?.to_string();
            for number in start.0 + 1..end.0 {
                code.push('\n');
                code.push_str(line(number)?);
            }
            code.push('\n');
            code.push_str(last.get(..from(last, end.1)?)?);
            Some(code)
        }
    }
}

#[derive(Deserialize)]
struct Position {
    line: u32,
    column: u32,
}

#[derive(Deserialize)]
struct Location {
    start: Position,
    end: Position,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaReport {
    schema_version: String,
    #[serde(default)]
    project_root: Option<String>,
    files: BTreeMap<String, SchemaFile>,
}

#[derive(Deserialize)]
struct SchemaFile {
    source: String,
    mutants: Vec<SchemaMutant>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaMutant {
    id: String,
    mutator_name: String,
    #[serde(default)]
    replacement: Option<String>,
    location: Location,
    status: String,
}

fn schema(value: serde_json::Value, root: &std::path::Path) -> Result<Report> {
    let report: SchemaReport =
        serde_json::from_value(value).context("the mutation testing report is malformed")?;
    let major = report.schema_version.split('.').next().unwrap_or_default();
    if !matches!(major, "1" | "2") {
        bail!(
            "the mutation testing report is of schema version {}; flot reads versions 1 and 2",
            report.schema_version
        );
    }
    // Paths are relative to the project root, which may be a package below
    // the flow root.
    let prefix = match &report.project_root {
        Some(project) => within(project, root).with_context(|| {
            format!("the report's project root {project} is not inside the flow root")
        })?,
        None => std::path::PathBuf::new(),
    };
    let mut tally = Report {
        caught: 0,
        untested: 0,
        survived: Vec::new(),
    };
    for (path, file) in &report.files {
        let file_name = if is_contained(path) {
            prefix.join(path)
        } else {
            within(path, root)
                .with_context(|| format!("the report names {path}, outside the flow root"))?
        };
        let file_name = file_name.to_string_lossy().replace('\\', "/");
        for mutant in &file.mutants {
            match mutant.status.as_str() {
                "Killed" | "Timeout" => tally.caught += 1,
                "CompileError" | "RuntimeError" | "Ignored" => tally.untested += 1,
                "Survived" | "NoCoverage" => {
                    let location = &mutant.location;
                    let code = span(
                        &file.source,
                        (location.start.line, location.start.column),
                        (location.end.line, location.end.column),
                    )
                    .with_context(|| {
                        format!(
                            "mutant {} of {path} is placed outside its source",
                            mutant.id
                        )
                    })?;
                    tally.survived.push(Survivor {
                        file: file_name.clone(),
                        mutation: match &mutant.replacement {
                            Some(replacement) => {
                                format!("{}: {}", mutant.mutator_name, replacement)
                            }
                            None => mutant.mutator_name.clone(),
                        },
                        code: digest::bytes(code.as_bytes()),
                        line: location.start.line,
                    });
                }
                "Pending" => bail!("mutant {} of {path} was never tested", mutant.id),
                other => bail!(
                    "mutant {} of {path} has the unknown status `{other}`",
                    mutant.id
                ),
            }
        }
    }
    tally.survived.sort();
    Ok(tally)
}

/// `path` relative to the flow root `root`: as it is when it is relative,
/// else what follows `root` in it. `None` when it is not inside.
fn within(path: &str, root: &std::path::Path) -> Option<std::path::PathBuf> {
    if is_contained(path) {
        return Some(path.into());
    }
    let rest = std::path::Path::new(path).strip_prefix(root).ok()?;
    let clean = rest.as_os_str().is_empty() || rest.to_str().is_some_and(is_contained);
    clean.then(|| rest.to_path_buf())
}

#[derive(Deserialize)]
struct CargoOutcomes {
    outcomes: Vec<CargoOutcome>,
}

#[derive(Deserialize)]
struct CargoOutcome {
    scenario: CargoScenario,
    summary: String,
}

#[derive(Deserialize)]
enum CargoScenario {
    Baseline,
    Mutant(CargoMutant),
}

#[derive(Deserialize)]
struct CargoMutant {
    name: String,
    file: String,
    span: Location,
}

fn cargo_mutants(
    value: serde_json::Value,
    planned: &[u8],
    source: &dyn Fn(&str) -> Option<String>,
) -> Result<Report> {
    let outcomes: CargoOutcomes =
        serde_json::from_value(value).context("cargo-mutants' outcomes.json is malformed")?;
    let planned: Vec<CargoMutant> =
        serde_json::from_slice(planned).context("cargo-mutants' mutants.json is malformed")?;
    let mut tested = BTreeSet::new();
    let mut tally = Report {
        caught: 0,
        untested: 0,
        survived: Vec::new(),
    };
    for outcome in outcomes.outcomes {
        let mutant = match outcome.scenario {
            CargoScenario::Baseline if outcome.summary == "Success" => continue,
            CargoScenario::Baseline => {
                bail!("the tests failed before any mutation, so no mutant was tested")
            }
            CargoScenario::Mutant(mutant) => mutant,
        };
        match outcome.summary.as_str() {
            "CaughtMutant" | "Timeout" => tally.caught += 1,
            "Unviable" => tally.untested += 1,
            "MissedMutant" => {
                let text = source(&mutant.file).with_context(|| {
                    format!(
                        "cannot read {}, which the report names; run cargo-mutants from the flow root",
                        mutant.file
                    )
                })?;
                let location = &mutant.span;
                let code = span(
                    &text,
                    (location.start.line, location.start.column),
                    (location.end.line, location.end.column),
                )
                .with_context(|| format!("{} is placed outside {}", mutant.name, mutant.file))?;
                // The name leads with where the mutant is, which other code
                // moves.
                let place = format!(
                    "{}:{}:{}: ",
                    mutant.file, location.start.line, location.start.column
                );
                tally.survived.push(Survivor {
                    mutation: mutant
                        .name
                        .strip_prefix(&place)
                        .unwrap_or(&mutant.name)
                        .to_string(),
                    file: mutant.file.clone(),
                    code: digest::bytes(code.as_bytes()),
                    line: location.start.line,
                });
            }
            other => bail!("{} has the unknown outcome `{other}`", mutant.name),
        }
        tested.insert(mutant.name);
    }
    if let Some(untested) = planned.iter().find(|mutant| !tested.contains(&mutant.name)) {
        let more = planned
            .iter()
            .filter(|mutant| !tested.contains(&mutant.name))
            .count()
            - 1;
        bail!(
            "{} was never tested{}",
            untested.name,
            if more == 0 {
                String::new()
            } else {
                format!(", nor {more} more")
            }
        );
    }
    tally.survived.sort();
    Ok(tally)
}

/// A surviving mutant a person accepts, and why.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allowed {
    pub file: String,
    pub mutation: String,
    pub code: String,
    pub reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowFile {
    #[serde(default)]
    allow: Vec<Allowed>,
}

/// Read an allow list: `[[allow]]` entries, each with a reason.
pub fn read_allow(text: &str) -> Result<Vec<Allowed>> {
    let file: AllowFile = toml::from_str(text).context("the allow list is malformed")?;
    for entry in &file.allow {
        if entry.reason.trim().is_empty() {
            bail!(
                "the allow list's entry for `{}` in {} gives no reason",
                entry.mutation,
                entry.file
            );
        }
    }
    Ok(file.allow)
}

/// Split the survivors into those no entry allows and those one does.
pub fn judge(survived: Vec<Survivor>, allowed: &[Allowed]) -> (Vec<Survivor>, Vec<Survivor>) {
    survived.into_iter().partition(|survivor| {
        !allowed.iter().any(|entry| {
            entry.file == survivor.file
                && entry.mutation == survivor.mutation
                && entry.code == survivor.code
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            error.contains(
                "replace Builds::work -> PathBuf with Default::default() was never tested"
            ),
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

    #[test]
    fn an_entry_allows_the_survivor_it_names_and_no_other() {
        let survivor = |mutation: &str, code: &str| Survivor {
            file: "src/a.rs".into(),
            mutation: mutation.into(),
            code: code.into(),
            line: 1,
        };
        let allowed = read_allow(
            "[[allow]]\nfile = \"src/a.rs\"\nmutation = \"replace > with >=\"\ncode = \"sha256:1\"\nreason = \"equivalent: the age is never equal\"\n",
        )
        .unwrap();
        let (left, kept) = judge(
            vec![
                survivor("replace > with >=", "sha256:1"),
                survivor("replace > with >=", "sha256:2"),
                survivor("replace > with <", "sha256:1"),
            ],
            &allowed,
        );
        assert_eq!(kept, [survivor("replace > with >=", "sha256:1")]);
        assert_eq!(left.len(), 2);
        assert_eq!(read_allow("").unwrap(), []);

        for (text, refusal) in [
            (
                "[[allow]]\nfile = \"a\"\nmutation = \"m\"\ncode = \"c\"\nreason = \" \"\n",
                "gives no reason",
            ),
            (
                "[[allow]]\nfile = \"a\"\nmutation = \"m\"\ncode = \"c\"\n",
                "missing field `reason`",
            ),
            (
                "[[allow]]\nfile = \"a\"\nmutation = \"m\"\ncode = \"c\"\nreason = \"r\"\nline = 3\n",
                "unknown field `line`",
            ),
        ] {
            let error = format!("{:#}", read_allow(text).unwrap_err());
            assert!(error.contains(refusal), "{text}: {error}");
        }
    }

    #[test]
    fn a_span_counts_characters_and_excludes_its_end() {
        assert_eq!(span("añb = 1", (1, 2), (1, 4)).as_deref(), Some("ñb"));
        assert_eq!(span("ab", (1, 1), (1, 3)).as_deref(), Some("ab"));
        assert_eq!(span("ab", (1, 1), (1, 4)), None);
        assert_eq!(span("ab", (2, 1), (2, 2)), None);
        assert_eq!(span("ab\ncd", (2, 1), (1, 2)), None);
        assert_eq!(span("ab", (0, 1), (1, 2)), None);
    }
}
