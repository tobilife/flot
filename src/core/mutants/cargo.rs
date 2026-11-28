//! cargo-mutants' `outcomes.json`: what testing each mutant came to,
//! checked against the plan in `mutants.json` so that a run stopped short
//! is not read as complete.

use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::place::{span, Location};
use super::{Report, Survivor};
use crate::core::digest;

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

pub(super) fn cargo_mutants(
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
