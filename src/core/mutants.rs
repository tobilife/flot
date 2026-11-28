//! What a mutation tester reports of a proof: the mutants it made of the
//! code, and which of them the proof's tests caught. A mutant that survived
//! is a change to the code the proof cannot tell from the code itself.
//!
//! Two reports are read: the mutation testing report schema (Stryker and
//! the tools that share it) and cargo-mutants' `outcomes.json`, with the
//! `mutants.json` beside it that says which mutants were to be tested. Both
//! place a mutant by 1-based line and column, its end excluded.

mod allow;
mod cargo;
mod place;
mod schema;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use cargo::cargo_mutants;
use schema::schema;

pub use allow::{judge, read_allow, Allowed};

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

#[cfg(test)]
mod tests;
