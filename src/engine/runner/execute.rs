//! Executing a step once: its environment, its command and proof, and the
//! receipt that records them, or that says why it never ran.

mod command;
mod findings;
mod mutation;
mod outputs;
mod proof;
mod revision;
mod scratch;
mod setup;
mod spend;
mod verify;

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;

use crate::core::flow::Sandbox;
use crate::core::metrics::Metrics;
use crate::core::receipt::{
    ClaimRecord, FileDigest, Observed, Receipt, ReceiptIdentity, SandboxRecord, StepStatus,
    ToolIdentity,
};
use crate::engine::claims;
use crate::engine::exec;
use crate::engine::sandbox;

use self::scratch::Scratch;
use self::spend::read_metrics;
use super::{Keyed, RunOptions, Runner};

impl Runner<'_> {
    /// The receipt of a step that never produced a result of its own:
    /// cancelled before it started, or failed because it could not run.
    /// Its limitations are its facts' and then `why`.
    pub(super) fn unrun_receipt(
        &self,
        unrun: Unrun,
        keyed: Keyed<'_>,
        why: String,
    ) -> Result<Receipt> {
        let (status, reason) = match unrun {
            Unrun::Cancelled => (StepStatus::Cancelled, "step was not run"),
            Unrun::Failed => (StepStatus::Failed, "step could not run"),
            Unrun::Unconfined => (StepStatus::Failed, "step could not run in its sandbox"),
        };
        let Keyed {
            step,
            step_key,
            facts,
            deps,
        } = keyed;
        let mut limitations = facts.limitations;
        limitations.push(why);
        let empty = self.store.put_bytes(b"")?;
        Ok(Receipt::seal(
            ReceiptIdentity {
                flow: self.flow.meta.name.clone(),
                step_id: step.id.clone(),
                step_key,
                command: step.run.clone(),
                status,
                exit_code: None,
                stdout_digest: empty.clone(),
                stderr_digest: empty,
                inputs: facts.inputs,
                verify_inputs: facts.verify_inputs,
                outputs: Vec::new(),
                changed: BTreeMap::new(),
                deps,
                claims: claims::not_run(step, reason),
                limitations,
                tool: ToolIdentity::default(),
                candidate: self.candidate.clone(),
                sandbox: None,
            },
            Observed {
                started_at_ms: exec::now_ms(),
                host_os: std::env::consts::OS.to_string(),
                ..Observed::default()
            },
        ))
    }

    /// The receipt of a step whose sandbox cannot be enforced here: a step
    /// that asked to be confined runs confined or not at all.
    fn unconfined(&self, keyed: Keyed<'_>, refused: Unenforceable) -> Result<Receipt> {
        let Unenforceable(error) = refused;
        let why = format!("could not run: its sandbox cannot be enforced here: {error:#}");
        self.unrun_receipt(Unrun::Unconfined, keyed, why)
    }

    /// Run a step once and settle its claims. `since` is the workspace before
    /// the step's first attempt, taken here when this is it.
    ///
    /// The phases, in order: set up where the command runs, run it, judge
    /// how it ended, keep what it produced, hold it to its `may_change`,
    /// bind what it changed, prove its claims, and seal the receipt. Each
    /// adds its limitations after those found before: their order is part
    /// of the receipt id.
    pub(super) fn execute(
        &self,
        keyed: Keyed<'_>,
        extra: &BTreeMap<String, String>,
        options: &RunOptions,
        since: &mut Option<Vec<FileDigest>>,
    ) -> Result<Receipt> {
        let step = keyed.step;
        let budget = self.flow.resolved_budget(step);
        let setup = match self.set_up(step, extra)? {
            Ok(setup) => setup,
            Err(refused) => return self.unconfined(keyed, refused),
        };
        let mut ran = match self.run_step(step, &setup, budget, options, since)? {
            Ok(ran) => ran,
            Err(refused) => return self.unconfined(keyed, refused),
        };
        let mut limitations = keyed.facts.limitations.clone();
        limitations.append(&mut ran.problems);
        let reported = read_metrics(&setup.metrics_path);
        let mut found = Findings::ended(limitations, step, budget, &ran.outcome, reported);
        let produced = self.keep_outputs(step, budget, &ran.outcome, &mut found)?;
        found.strayed(step, ran.strayed.as_deref());
        let after = self.facts(step);
        self.bind_changes(&keyed, &after, &mut ran.changed, &mut found.limitations);
        self.settle_claims(&keyed, &after, &produced.files, budget, options, &mut found);
        Ok(self.seal(keyed, setup, ran, produced, found))
    }

    /// The receipt of the execution, as its phases found it. Confined in a
    /// scratch directory, the step is done with it once this is sealed.
    fn seal(
        &self,
        keyed: Keyed<'_>,
        setup: Setup<'_>,
        ran: Ran,
        produced: Produced,
        found: Findings,
    ) -> Receipt {
        let Keyed {
            step,
            step_key,
            facts,
            deps,
        } = keyed;
        Receipt::seal(
            ReceiptIdentity {
                flow: self.flow.meta.name.clone(),
                step_id: step.id.clone(),
                step_key,
                command: step.run.clone(),
                status: found.status,
                exit_code: ran.outcome.exit_code,
                stdout_digest: produced.stdout_digest,
                stderr_digest: produced.stderr_digest,
                inputs: facts.inputs,
                verify_inputs: facts.verify_inputs,
                outputs: produced.files,
                changed: ran.changed,
                deps,
                claims: found.claims,
                limitations: found.limitations,
                tool: ToolIdentity::default(),
                candidate: self.candidate.clone(),
                sandbox: setup.confined.map(|(policy, prepared)| SandboxRecord {
                    network: policy.network,
                    write: policy.write.clone(),
                    mechanism: prepared.mechanism,
                }),
            },
            Observed {
                started_at_ms: ran.outcome.started_at_ms,
                duration_ms: ran.outcome.duration_ms,
                host_os: std::env::consts::OS.to_string(),
                stdout_inline: produced.stdout_inline,
                stdout_truncated: produced.stdout_truncated,
                stderr_inline: produced.stderr_inline,
                stderr_truncated: produced.stderr_truncated,
                stdout_bytes: ran.outcome.stdout.len(),
                stderr_bytes: ran.outcome.stderr.len(),
                metrics: found.metrics,
                metrics_notes: found.metrics_notes,
            },
        )
    }
}

/// How a step that never produced a result of its own ended.
pub(super) enum Unrun {
    /// The run did not start it.
    Cancelled,
    /// It could not run.
    Failed,
    /// Its sandbox cannot be enforced here.
    Unconfined,
}

/// Where and how a step's command runs.
struct Setup<'s> {
    env: BTreeMap<String, String>,
    /// Where the command reports what it spent.
    metrics_path: PathBuf,
    /// The confined step's own directory for its metrics and temporary
    /// files, removed with the setup.
    _scratch: Option<Scratch>,
    /// The sandbox the step asked for, prepared.
    confined: Option<(&'s Sandbox, sandbox::Prepared)>,
}

/// Why a step's sandbox cannot be enforced here.
struct Unenforceable(anyhow::Error);

/// What running a step's command left: how it ended, and what it changed.
struct Ran {
    outcome: exec::Outcome,
    /// What the snapshots around a step with `may_change` could not read.
    problems: Vec<String>,
    /// Files it changed outside its `may_change`, when it declares one.
    strayed: Option<Vec<String>>,
    /// What every attempt so far changed, when the receipt binds that.
    changed: BTreeMap<String, Option<String>>,
}

/// What a step produced, kept in the store: its output streams, with what
/// its receipt shows of them inline, and the files of its declared outputs.
struct Produced {
    stdout_digest: String,
    stderr_digest: String,
    stdout_inline: String,
    stdout_truncated: bool,
    stderr_inline: String,
    stderr_truncated: bool,
    files: Vec<FileDigest>,
}

/// What the phases after a step's command find: its status, its claims,
/// and its limitations in the order found, which is part of the receipt
/// id; and what the step and its proofs spent.
struct Findings {
    status: StepStatus,
    /// A failure that `allow_failure` let pass: the step is `ok`, and
    /// proves nothing.
    tolerated: bool,
    claims: Vec<ClaimRecord>,
    limitations: Vec<String>,
    metrics: Option<Metrics>,
    metrics_notes: Vec<String>,
    /// Processes that left their group, from the step and from its proofs.
    escaped: Vec<exec::Escaped>,
}
