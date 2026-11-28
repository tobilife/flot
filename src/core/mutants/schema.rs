//! The mutation testing report schema, which Stryker and the tools that
//! share it write: each file's source, and what became of every mutant
//! made of it.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::place::{span, Location};
use super::{Report, Survivor};
use crate::core::digest;
use crate::core::flow::is_contained;

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

pub(super) fn schema(value: serde_json::Value, root: &std::path::Path) -> Result<Report> {
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
