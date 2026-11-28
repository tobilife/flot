//! Putting a run's manifest together from its settled steps: each step's
//! entry with the attempts and candidates it ran, what the run spent, and
//! the limitations a reader of its verdict must know.

mod entries;
mod limitations;

use std::collections::HashMap;
use std::time::Instant;

use anyhow::Result;

use crate::core::metrics::Spend;
use crate::core::receipt::{verdict_for, ClaimCounts, Partial, RunManifest, RunStep, FLOT_VERSION};
use crate::engine::exec;

use super::{RunReport, Runner, Settled, StepReport};

/// A run's manifest as it is put together, one settled step at a time.
#[derive(Default)]
pub(super) struct Assembly {
    /// How many steps could run at once.
    jobs: usize,
    ceiling_micro_usd: Option<u64>,
    /// Set when the selection leaves steps out.
    partial: Option<Partial>,
    steps: Vec<RunStep>,
    reports: Vec<StepReport>,
    totals: ClaimCounts,
    spend: Spend,
    /// Executions whose cost is unknown: they reported nothing, or no cost.
    silent: Vec<String>,
    unpriced: Vec<String>,
    limitations: Vec<String>,
}

impl Runner<'_> {
    /// Each settled step of `order`, in that order and never in completion
    /// order, so the run root does not depend on which worker finished
    /// first; then the limitations of the run as a whole.
    pub(super) fn assemble(
        &self,
        order: &[String],
        settled: &HashMap<String, Settled>,
        jobs: usize,
        ceiling_micro_usd: Option<u64>,
    ) -> Result<Assembly> {
        // A selection that leaves steps out is recorded, so no report reads
        // its verdict as the flow's.
        let partial = (order.len() < self.flow.steps.len()).then_some(Partial {
            selected: order.len(),
            of: self.flow.steps.len(),
        });
        let mut assembly = Assembly {
            jobs,
            ceiling_micro_usd,
            partial,
            ..Assembly::default()
        };
        let proofs_before = self.proofs_before(order);
        for step_id in order {
            let entry = settled
                .get(step_id)
                .expect("every selected step settles exactly once");
            let limitations = self.step_limitations(step_id, entry, &proofs_before);
            assembly.add(self.store, step_id, entry, limitations)?;
        }

        assembly.limitations.extend(self.proof_limitations(order));
        if let Some(partial) = partial {
            assembly.limitations.push(format!(
                "partial run: {} of {} steps selected",
                partial.selected, partial.of
            ));
        }
        assembly.cost_limitations();
        assembly
            .limitations
            .extend(self.output_conflicts(order, settled)?);
        if jobs > 1 {
            assembly.limitations.push(format!(
                "up to {jobs} steps ran concurrently; flot checks the outputs steps declare, but cannot see files they write without declaring them"
            ));
        }
        Ok(assembly)
    }

    /// The run's manifest and report, sealed by the root of its steps'
    /// receipts.
    pub(super) fn report(
        &self,
        mut assembly: Assembly,
        started_at_ms: u64,
        started: Instant,
    ) -> Result<RunReport> {
        let (started_at_ms, note) =
            after_the_run_before(started_at_ms, self.store.newest_local_start_ms()?);
        assembly.limitations.extend(note);
        let pairs: Vec<(String, String)> = assembly
            .steps
            .iter()
            .map(|step| (step.step_id.clone(), step.receipt_id.clone()))
            .collect();
        let root_digest = RunManifest::root_of(&pairs);
        let verdict = verdict_for(&assembly.steps, assembly.totals);
        let duration_ms = exec::millis(started.elapsed());
        // What the guard refused since the run before this one finished,
        // agents this run started included.
        let previous_end = self
            .store
            .resolve_run("latest")
            .map(|previous| previous.started_at_ms + previous.duration_ms)
            .unwrap_or(0);
        let refusals = self
            .store
            .refusals(previous_end, started_at_ms + duration_ms)?;
        let manifest = RunManifest {
            run_id: RunManifest::id_for(started_at_ms, &root_digest),
            flow: self.flow.meta.name.clone(),
            flow_digest: self.flow.digest(),
            root_digest,
            verdict,
            partial: assembly.partial,
            steps: assembly.steps,
            claims: assembly.totals,
            limitations: assembly.limitations,
            max_parallel: assembly.jobs,
            spend: assembly.spend,
            max_cost_micro_usd: assembly.ceiling_micro_usd,
            started_at_ms,
            duration_ms,
            flot_version: FLOT_VERSION.to_string(),
            refusals,
        };
        Ok(RunReport {
            manifest,
            steps: assembly.reports,
            vouched: Vec::new(),
        })
    }
}

/// When a run starts, kept after the newest run this host recorded. Run ids
/// begin with the start, and `latest~N` and the run list count back in that
/// order, so a clock set back since then would otherwise put this run behind
/// one recorded before it while `latest` still names this one. The start is
/// then recorded 1 ms after that run's, and the run says so.
fn after_the_run_before(now_ms: u64, previous_start_ms: Option<u64>) -> (u64, Option<String>) {
    match previous_start_ms {
        Some(previous) if now_ms <= previous => (
            previous + 1,
            Some(format!(
                "this host's clock read no later than the start of the newest run recorded here ({} ms earlier); the start is recorded 1 ms after it, so runs stay in the order they were recorded",
                previous - now_ms
            )),
        ),
        _ => (now_ms, None),
    }
}

#[cfg(test)]
mod tests;
