//! What a step declares: the command it runs, the files it reads and
//! writes, what it claims and how that is proved; and the rules step ids
//! and declared paths follow.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::Budget;

/// The variable that names the candidate a step runs as. It is part of the
/// candidate's declared environment, so each candidate has its own key.
pub const CANDIDATE_ENV: &str = "FLOT_CANDIDATE";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    /// Command executed through `sh -e -c` from the flow root: the first failing
    /// line fails the step.
    pub run: String,
    #[serde(default)]
    pub needs: Vec<String>,
    /// Glob patterns, relative to the flow root, whose content identity feeds
    /// the step key. Declaring inputs is what makes a cache hit trustworthy.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// What this step asserts once it succeeds. A claim is only ever `verified`
    /// when `verify` exits zero.
    #[serde(default)]
    pub claims: Vec<String>,
    /// Independent command that proves the claims. No verify command means the
    /// claims stay `unverified` and the run cannot be reported as passed.
    #[serde(default)]
    pub verify: Option<String>,
    /// Glob patterns for the files the proof reads, such as an acceptance test.
    /// Their content feeds the step key, so changing the proof re-proves the
    /// claim instead of reusing a verdict the new proof never gave. They are
    /// also guarded: if the step itself changes them, the claim cannot verify.
    #[serde(default)]
    pub verify_inputs: Vec<String>,
    /// Glob patterns for the files the step produces. Each one must match a
    /// file once the step succeeds; their content is recorded in the receipt
    /// and kept in the store, and a reused receipt puts them back.
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub budget: Budget,
    /// When true a nonzero exit does not fail the run, but the step's claims
    /// are still not verified.
    #[serde(default)]
    pub allow_failure: bool,
    /// How many times the step may run in one flot run before it holds: it
    /// succeeds and every claim with a proof command is verified. Each later
    /// attempt sees why the one before did not hold in `$FLOT_FEEDBACK`. Like
    /// a budget, not part of the step key: work that held is the same work
    /// whichever attempt it took.
    #[serde(default)]
    pub attempts: Option<u32>,
    /// Names of interchangeable ways to do the step, such as different agents
    /// or models. Each runs in its own copy of the workspace with
    /// `$FLOT_CANDIDATE` naming it, and is proved there; of those that hold,
    /// the one that reported the lowest cost is kept and its outputs are
    /// written back. The others stay as evidence.
    #[serde(default)]
    pub candidates: Vec<String>,
    /// The files the step may change, as globs relative to the flow root. When
    /// set, a step that creates, edits or deletes any other file (outside its
    /// `outputs`, what git ignores and the store) fails, and the step runs
    /// with no other step beside it so that what changed is its own doing.
    #[serde(default)]
    pub may_change: Vec<String>,
    /// Confinement the kernel enforces on the step: no network, writes only
    /// beneath some paths. Where it cannot be enforced the step is not run.
    #[serde(default)]
    pub sandbox: Option<Sandbox>,
    /// The proof must fail before the change: once it passes, it runs again
    /// on a copy of the work with every change since the revision proofs
    /// are checked against undone, and the claims verify only if it fails
    /// there. A proof that passes either way shows nothing this change did.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fails_before: bool,
    /// Proof files held to the revision proofs are checked against, as globs
    /// relative to the flow root, such as the tests: once the proof passes,
    /// it runs again on a copy of the work with these files as the revision
    /// has them (edited ones put back, deleted ones restored, new ones
    /// removed), and the claims verify only if it passes there too. Work
    /// that rewrites or drops a test to pass stays unverified until the
    /// rewritten test is at the revision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<String>,
    /// A mutation tester the proof must stand up to: once the proof passes,
    /// this runs and the claims verify only if every mutant its report
    /// lists is caught, or allowed by a person in the allow list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutants: Option<Mutants>,
    /// The step runs on every run and its receipt is never reused: for a
    /// probe of what no key covers, such as the toolchain on `PATH`. Its
    /// receipt id follows what it printed and produced, so the steps that
    /// need it run again only when that changed. Not part of the step key.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub always: bool,
}

/// A mutation tester run against a step's proof, and what it reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mutants {
    /// Command run from the flow root once the proof passes, with
    /// `$FLOT_MUTANTS_DIR`, an empty directory, to write its report in.
    pub run: String,
    /// Path of the report in `$FLOT_MUTANTS_DIR`: a mutation testing report
    /// (the schema Stryker writes) or cargo-mutants' `outcomes.json`.
    pub report: String,
    /// The allow list, relative to the flow root: mutants a person accepts
    /// as surviving, each with a reason. A proof input of the step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sandbox {
    /// Whether the step may use the network. A confined step may not unless
    /// this says so.
    #[serde(default)]
    pub network: bool,
    /// Paths, relative to the flow root, the step may write beneath; absent,
    /// writes are not restricted. The temporary directory and `/dev` are
    /// always writable.
    #[serde(default)]
    pub write: Option<Vec<String>>,
}

impl Step {
    /// The step as it runs for one candidate: its environment names it.
    pub fn as_candidate(&self, name: &str) -> Step {
        let mut step = self.clone();
        step.env.insert(CANDIDATE_ENV.into(), name.into());
        step
    }

    /// The patterns of the files the proof reads: `verify_inputs`, and the
    /// mutants' allow list, whose entries decide the verdict as much.
    pub fn proof_inputs(&self) -> Vec<String> {
        let mut patterns = self.verify_inputs.clone();
        if let Some(allow) = self
            .mutants
            .as_ref()
            .and_then(|mutants| mutants.allow.as_ref())
        {
            if !patterns.contains(allow) {
                patterns.push(allow.clone());
            }
        }
        patterns
    }

    /// How many attempts one run may make at this step.
    pub fn attempts(&self) -> u32 {
        self.attempts.unwrap_or(1)
    }

    /// Whether `path`, relative to the flow root, is one of this step's
    /// declared outputs. A receipt naming any other file is not this step's to
    /// restore, whatever it claims.
    pub fn declares_output(&self, path: &str) -> bool {
        let options = glob::MatchOptions {
            require_literal_separator: true,
            ..glob::MatchOptions::new()
        };
        is_contained(path)
            && self.outputs.iter().any(|pattern| {
                glob::Pattern::new(pattern).is_ok_and(|pattern| pattern.matches_with(path, options))
            })
    }
}

/// Step ids name receipts in output and are compared across hosts, so they are
/// restricted to a portable, printable alphabet.
pub fn is_valid_step_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A relative path made only of plain components: nothing absolute, no `..`,
/// no `.`. flot writes outputs back to these paths, so they must not escape.
pub fn is_contained(path: &str) -> bool {
    let path = std::path::Path::new(path);
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}
