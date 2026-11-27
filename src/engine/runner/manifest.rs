//! Putting a run's manifest together from its settled steps: each step's
//! entry with the attempts and candidates it ran, what the run spent, and
//! the limitations a reader of its verdict must know.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::Result;

use crate::core::flow::Step;
use crate::core::metrics::{self, Spend};
use crate::core::receipt::{
    verdict_for, ClaimCounts, Partial, Receipt, ReceiptIdentity, RunAttempt, RunCandidate,
    RunManifest, RunStep, StepStatus, FLOT_VERSION,
};
use crate::engine::exec;
use crate::engine::keying;
use crate::store::Store;

use super::{Contender, RunReport, Runner, Settled, StepReport};

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

    /// What a reader of a step's verdict in this run must know: its
    /// receipt's limitations and the run's notes on it, then whether its
    /// proof changed since it last ran here, which candidates ran, and how
    /// many attempts it took.
    fn step_limitations(
        &self,
        step_id: &str,
        entry: &Settled,
        proofs_before: &HashMap<String, (String, Option<ReceiptIdentity>)>,
    ) -> Vec<String> {
        let receipt = &entry.receipt;
        let step = self.flow.step(step_id).expect("validated step");
        let mut limitations = receipt.identity.limitations.clone();
        limitations.extend(entry.notes.iter().cloned());
        if let Some((run_id, Some(earlier))) = proofs_before.get(step_id) {
            let changed = proof_changes(earlier, &receipt.identity);
            if !changed.is_empty() {
                limitations.push(format!(
                    "its proof changed since run {run_id} ({}); read the change before trusting the verdict",
                    changed.join(", ")
                ));
            }
        }
        if !entry.cached && receipt.identity.candidate.is_some() {
            limitations.push(candidates_told(step, entry));
        }
        if !entry.attempts.is_empty() {
            limitations.push(attempts_told(step, entry));
        }
        limitations
    }

    /// What the run's proofs were held to: the revision of its baseline, and
    /// the steps with claims there that this flow lacks; or, without one,
    /// nothing that stops an edited proof from passing.
    fn proof_limitations(&self, order: &[String]) -> Vec<String> {
        let mut limitations = Vec::new();
        if let Some(baseline) = self.baseline {
            limitations.push(format!("proofs checked against {}", baseline.label()));
            let missing = baseline.missing_steps(|id| self.flow.step(id).is_some());
            if !missing.is_empty() {
                limitations.push(format!(
                    "steps with claims at {} are not in this flow: {}; their claims are not made",
                    baseline.label(),
                    missing.join(", ")
                ));
            }
        } else if order.iter().any(|step_id| {
            self.flow
                .step(step_id)
                .is_some_and(|step| step.verify.is_some())
        }) {
            limitations.push(
                "proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)"
                    .into(),
            );
        }
        limitations
    }

    /// For each step of `order` with a proof, the newest run executed here
    /// that has it, and its receipt there (`None` when that receipt is
    /// unreadable). Imported runs are not this host's, and a run of another
    /// flow is not this step's.
    fn proofs_before(
        &self,
        order: &[String],
    ) -> HashMap<String, (String, Option<ReceiptIdentity>)> {
        let mut found = HashMap::new();
        // Held to no revision, a proof can be edited along with the work it
        // judges; what it read last time here is the one thing to compare.
        if self.baseline.is_some() {
            return found;
        }
        // Only a step with a proof has one to compare, and the store is read
        // until each such step is found.
        let steps: Vec<String> = order
            .iter()
            .filter(|step_id| {
                self.flow
                    .step(step_id)
                    .is_some_and(|step| step.verify.is_some())
            })
            .cloned()
            .collect();
        for run_id in self.store.list_runs().unwrap_or_default() {
            if found.len() == steps.len() {
                break;
            }
            if self.store.is_imported(&run_id) {
                continue;
            }
            let Ok(run) = self.store.resolve_run(&run_id) else {
                continue;
            };
            if run.flow != self.flow.meta.name {
                continue;
            }
            for entry in run.steps {
                if found.contains_key(&entry.step_id) || !steps.contains(&entry.step_id) {
                    continue;
                }
                let earlier = self
                    .store
                    .load_receipt(&entry.receipt_id)
                    .ok()
                    .map(|receipt| receipt.identity);
                found.insert(entry.step_id, (run_id.clone(), earlier));
            }
        }
        found
    }
}

impl Assembly {
    /// Add a settled step: its manifest entry and its report, what its
    /// executions spent, and its `limitations`, each named by the step.
    fn add(
        &mut self,
        store: &Store,
        step_id: &str,
        entry: &Settled,
        limitations: Vec<String>,
    ) -> Result<()> {
        let receipt = &entry.receipt;
        let cached = entry.cached;
        let counts = receipt.claim_counts();
        self.totals.add(counts);
        let mut attempts = Vec::new();
        for earlier in &entry.attempts {
            attempts.push(self.executed(store, earlier)?);
        }
        let mut candidates = Vec::new();
        for candidate in &entry.candidates {
            let mut attempts = Vec::new();
            for earlier in candidate.attempts.iter().chain([&candidate.receipt]) {
                attempts.push(self.executed(store, earlier)?);
            }
            let last = attempts.pop().expect("a candidate has a last receipt");
            candidates.push(RunCandidate {
                name: candidate.name.clone(),
                receipt_id: last.receipt_id,
                observed_digest: last.observed_digest,
                attempts,
            });
        }
        if cached {
            self.spend.add_reused(receipt.observed.metrics);
        } else if receipt.identity.status != StepStatus::Cancelled {
            self.spend.add_executed(receipt.observed.metrics);
            self.unknown_cost(receipt);
        }
        for limitation in &limitations {
            self.limitations
                .push(format!("{}: {}", step_id, limitation));
        }
        if !cached {
            for note in &receipt.observed.metrics_notes {
                self.limitations.push(format!("{}: {}", step_id, note));
            }
        }
        self.steps.push(RunStep {
            step_id: step_id.to_string(),
            receipt_id: receipt.receipt_id.clone(),
            status: receipt.identity.status,
            cached,
            claims: counts,
            observed_digest: store.put_observation(&receipt.observed)?,
            attempts,
            candidates,
            vouched_by: entry.vouched_by.clone(),
        });
        self.reports.push(StepReport {
            step_id: step_id.to_string(),
            receipt_id: receipt.receipt_id.clone(),
            stdout_digest: receipt.identity.stdout_digest.clone(),
            status: receipt.identity.status,
            cached,
            claims: counts,
            duration_ms: receipt.observed.duration_ms,
            stdout_inline: receipt.observed.stdout_inline.clone(),
            stdout_truncated: receipt.observed.stdout_truncated,
            stderr_inline: receipt.observed.stderr_inline.clone(),
            stderr_truncated: receipt.observed.stderr_truncated,
            limitations,
            metrics: receipt.observed.metrics,
            outputs: receipt.identity.outputs.clone(),
            candidate: receipt.identity.candidate.clone(),
            attempts: if cached || receipt.identity.status == StepStatus::Cancelled {
                0
            } else {
                entry.attempts.len() + 1
            },
        });
        Ok(())
    }

    /// Count what one execution of a step spent, and record it as an attempt.
    fn executed(&mut self, store: &Store, receipt: &Receipt) -> Result<RunAttempt> {
        self.spend.add_executed(receipt.observed.metrics);
        self.unknown_cost(receipt);
        Ok(RunAttempt {
            receipt_id: receipt.receipt_id.clone(),
            observed_digest: store.put_observation(&receipt.observed)?,
        })
    }

    /// Note an execution whose cost is unknown, named by its step and, for a
    /// candidate, the candidate.
    fn unknown_cost(&mut self, receipt: &Receipt) {
        let label = match &receipt.identity.candidate {
            Some(name) => format!("{} ({name})", receipt.identity.step_id),
            None => receipt.identity.step_id.clone(),
        };
        match receipt.observed.metrics {
            None => self.silent.push(label),
            Some(metrics) if metrics.cost_micro_usd.is_none() => self.unpriced.push(label),
            Some(_) => {}
        }
    }

    /// Under a cost ceiling: the executions it could not see, and how far
    /// the run went past it.
    fn cost_limitations(&mut self) {
        let Some(ceiling) = self.ceiling_micro_usd else {
            return;
        };
        if !self.silent.is_empty() {
            self.limitations.push(format!(
                "{} executed step(s) reported no metrics ({}); the cost ceiling cannot see what they spent",
                self.silent.len(),
                self.silent.join(", ")
            ));
        }
        if !self.unpriced.is_empty() {
            self.limitations.push(format!(
                "{} executed step(s) reported no cost ({}); the cost ceiling cannot see what they spent",
                self.unpriced.len(),
                self.unpriced.join(", ")
            ));
        }
        if self.spend.cost_micro_usd > ceiling {
            self.limitations.push(format!(
                "spent {} against a {} ceiling: the ceiling is checked before each step starts, so steps already running when it was crossed finished",
                metrics::format_usd(self.spend.cost_micro_usd),
                metrics::format_usd(ceiling)
            ));
        }
    }
}

/// Which candidates of a step ran, in flow order, how each ended and what
/// it cost, and which was kept.
fn candidates_told(step: &Step, entry: &Settled) -> String {
    let receipt = &entry.receipt;
    let kept = Contender {
        name: receipt.identity.candidate.clone().unwrap_or_default(),
        receipt: receipt.clone(),
        attempts: entry.attempts.clone(),
    };
    let told: Vec<String> = step
        .candidates
        .iter()
        .filter_map(|name| {
            std::iter::once(&kept)
                .chain(&entry.candidates)
                .find(|ran| &ran.name == name)
        })
        .map(|ran| {
            let tries = ran.attempts.len() + 1;
            format!(
                "{} {}{} ({})",
                ran.name,
                outcome(&ran.receipt),
                if tries > 1 {
                    format!(" after {tries} attempts")
                } else {
                    String::new()
                },
                ran.cost_micro_usd()
                    .map(metrics::format_usd)
                    .unwrap_or_else(|| "cost unknown".into())
            )
        })
        .collect();
    format!("candidates {}; kept {}", told.join(", "), kept.name)
}

/// How each attempt at a step ended, of the attempts it is allowed.
fn attempts_told(step: &Step, entry: &Settled) -> String {
    let mut told: Vec<String> = entry
        .attempts
        .iter()
        .enumerate()
        .map(|(number, earlier)| format!("{} {}", number + 1, outcome(earlier)))
        .collect();
    told.push(format!("{} {}", told.len() + 1, outcome(&entry.receipt)));
    format!(
        "took {} attempts of {} allowed: {}",
        told.len(),
        step.attempts(),
        told.join(", ")
    )
}

/// What differs between the proofs two receipts of a step record: the files
/// it read, its command and its claims. A command neither receipt records is
/// not compared.
fn proof_changes(earlier: &ReceiptIdentity, now: &ReceiptIdentity) -> Vec<String> {
    let mut changed = keying::changed_paths(&earlier.verify_inputs, &now.verify_inputs);
    let command = |identity: &ReceiptIdentity| {
        identity
            .claims
            .iter()
            .find_map(|claim| claim.verify_command.clone())
    };
    if let (Some(before), Some(after)) = (command(earlier), command(now)) {
        if before != after {
            changed.push("the verify command".into());
        }
    }
    if !earlier
        .claims
        .iter()
        .map(|claim| &claim.text)
        .eq(now.claims.iter().map(|claim| &claim.text))
    {
        changed.push("the claims".into());
    }
    changed
}

/// How one attempt ended, in a word or two: `held`, or why not.
fn outcome(receipt: &Receipt) -> String {
    let counts = receipt.claim_counts();
    if !receipt.identity.status.is_ok() {
        match receipt.identity.exit_code {
            Some(code) => format!("{} (exit {code})", receipt.identity.status.label()),
            None => receipt.identity.status.label().to_string(),
        }
    } else if counts.refuted > 0 {
        "refuted".into()
    } else if !receipt.is_reusable() {
        "unverified".into()
    } else {
        "held".into()
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
mod tests {
    use super::after_the_run_before;

    #[test]
    fn a_run_starts_after_the_one_recorded_before_it() {
        assert_eq!(after_the_run_before(500, None), (500, None));
        assert_eq!(after_the_run_before(500, Some(499)), (500, None));
        let (start, note) = after_the_run_before(500, Some(500));
        assert_eq!(start, 501);
        assert!(note.unwrap().contains("(0 ms earlier)"));
        let (start, note) = after_the_run_before(300, Some(1000));
        assert_eq!(start, 1001);
        assert!(note.unwrap().contains("(700 ms earlier)"));
    }
}
