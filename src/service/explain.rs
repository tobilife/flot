//! Why a recorded run did not pass, at a glance: for the run and for each
//! step that did not hold, what kept it from holding and what to read or do
//! next. The same explanation reaches the CLI and MCP; it is read from the
//! run, its receipts and the check `verify` makes, and changes nothing.

mod causes;
#[cfg(test)]
mod tests;

use anyhow::{bail, Result};
use serde::Serialize;

use crate::core::receipt::StepStatus;
use crate::service::audit::Outcome;
use crate::service::Workspace;

use causes::causes;

/// What kept something from holding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The run's evidence does not authenticate.
    Broken,
    /// Only some of the flow's steps ran.
    Partial,
    /// The change to the proofs since the revision they are held to is all
    /// that keeps the run from passing.
    OnlyProofsDiffer,
    /// A receipt no longer matches its body.
    Altered,
    /// The step's command failed or could not run.
    Failed,
    /// The step was stopped at its timeout.
    TimedOut,
    /// The step did not run because a step it needs did not hold.
    NotRun,
    /// The step asked for a sandbox this host cannot enforce.
    Sandbox,
    /// The step's proof differs from the revision proofs are held to.
    ProofChanged,
    /// No verify command proves the claim.
    NoProof,
    /// The proof rejected the claim.
    Refuted,
    /// The proof fails with the files the step holds as the revision has them.
    Held,
    /// The proof passes with the change undone, so it does not test it.
    FailsBefore,
    /// Mutants the proof did not catch, or a mutation run that did not hold.
    Mutants,
    /// The proof did not finish, or something else kept it from proving.
    Unproven,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cause {
    pub kind: Kind,
    pub detail: String,
    /// The command to read more with, or what to do about it.
    pub next: String,
}

impl Cause {
    fn new(kind: Kind, detail: impl Into<String>, next: impl Into<String>) -> Self {
        Cause {
            kind,
            detail: detail.into(),
            next: next.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StepExplanation {
    pub step_id: String,
    pub status: StepStatus,
    pub cached: bool,
    /// Empty when the step held.
    pub causes: Vec<Cause>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Explanation {
    pub run_id: String,
    pub result: Outcome,
    /// What keeps the run as a whole from passing.
    pub causes: Vec<Cause>,
    /// The steps that did not hold, in the run's order, or the one step
    /// asked about whether it held or not.
    pub steps: Vec<StepExplanation>,
}

impl Workspace {
    /// Explain `run`, or only its step `step`. Proofs are held to the
    /// flow's `proofs_from`, as `verify` holds them.
    pub fn explain(&self, run: &str, step: Option<&str>) -> Result<Explanation> {
        let audit = self.verify(run, None)?;
        let store = self.store()?;
        let manifest = store.resolve_run(run)?;
        if let Some(step) = step {
            if !manifest.steps.iter().any(|entry| entry.step_id == step) {
                let known: Vec<&str> = manifest.steps.iter().map(|s| s.step_id.as_str()).collect();
                bail!(
                    "run {} has no step `{step}`; its steps are {}",
                    manifest.run_id,
                    known.join(", ")
                );
            }
        }
        // The commands suggested name the run as it was asked for.
        let named = if run == "latest" {
            String::new()
        } else {
            format!(" {run}")
        };
        let result = audit.result();
        let mut run_causes = Vec::new();
        match result {
            Outcome::Broken => run_causes.push(Cause::new(
                Kind::Broken,
                "the run's evidence does not authenticate; nothing it says can be believed",
                format!("flot verify{named}"),
            )),
            Outcome::Partial => run_causes.push(Cause::new(
                Kind::Partial,
                "only some of the flow's steps ran, so the flow as a whole is not proven",
                "flot run",
            )),
            _ => {}
        }
        if audit.only_proofs_differ() {
            run_causes.push(Cause::new(
                Kind::OnlyProofsDiffer,
                "no step failed, and every claim left unverified is one of a step whose proof changed; a person reviews that change, which no run proves",
                format!("flot verify{named}"),
            ));
        }
        let mut steps = Vec::new();
        for entry in manifest
            .steps
            .iter()
            .filter(|entry| step.is_none_or(|step| entry.step_id == step))
        {
            let found = match store.load_receipt(&entry.receipt_id) {
                Ok(receipt) if receipt.is_authentic() => causes(&receipt, &named),
                Ok(_) | Err(_) => vec![Cause::new(
                    Kind::Altered,
                    "its receipt is missing or does not match its body",
                    format!("flot verify{named}"),
                )],
            };
            if step.is_some() || !found.is_empty() {
                steps.push(StepExplanation {
                    step_id: entry.step_id.clone(),
                    status: entry.status,
                    cached: entry.cached,
                    causes: found,
                });
            }
        }
        Ok(Explanation {
            run_id: manifest.run_id,
            result,
            causes: run_causes,
            steps,
        })
    }
}
