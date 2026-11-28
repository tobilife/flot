//! Running the selected steps: the scheduler hands each ready step its
//! work, and each settles as cancelled, reused or freshly executed.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::core::flow::Step;
use crate::core::metrics;
use crate::core::receipt::{candidate_rank, DepRef, Receipt};
use crate::engine::exec;
use crate::engine::keying;
use crate::engine::progress::Event;
use crate::engine::scheduler;

use super::execute::Unrun;
use super::{CostGate, Keyed, RunOptions, RunReport, Runner, Settled};

/// Everything a worker needs to settle one step, computed from its settled
/// dependencies on the scheduling thread.
struct Work<'f> {
    step: &'f Step,
    blocked: Vec<String>,
    deps: Vec<DepRef>,
    gate: Option<CostGate>,
}

impl<'a> Runner<'a> {
    pub fn run(&self, options: &RunOptions) -> Result<RunReport> {
        if options.jobs == Some(0) {
            bail!("jobs must be at least 1");
        }
        let order = self.selection(options)?;
        let jobs = options.jobs.or(self.flow.meta.max_parallel).unwrap_or(1);
        let ceiling_micro_usd = match options.max_cost_usd.or(self.flow.meta.max_cost_usd) {
            Some(dollars) => {
                Some(metrics::usd_to_micro(dollars).context("the run cost ceiling is invalid")?)
            }
            None => None,
        };
        // Held until the manifest is written: until then nothing names this
        // run's receipts, and gc must not collect them.
        let _lock = self.store.lock(false)?;
        // Held elsewhere, the builds are pruned by a later run.
        let _ = self.store.prune_builds();
        let started_at_ms = exec::now_ms();
        let started = std::time::Instant::now();
        options.progress.emit(|| Event::RunStarted {
            steps: order.len(),
            jobs,
        });

        let needs: BTreeMap<String, Vec<String>> = order
            .iter()
            .map(|step_id| {
                let step = self.flow.step(step_id).expect("validated step");
                (step_id.clone(), step.needs.clone())
            })
            .collect();
        let settled = scheduler::run_dag(
            &order,
            &needs,
            jobs,
            |step_id, settled: &HashMap<String, Settled>| {
                self.prepare(step_id, settled, ceiling_micro_usd)
            },
            |work| self.settle(work, options),
        )?;
        options.progress.emit(|| Event::RunFinished {
            duration_ms: exec::millis(started.elapsed()),
        });

        let assembly = self.assemble(&order, &settled, jobs, ceiling_micro_usd)?;
        let report = self.report(assembly, started_at_ms, started)?;
        self.store.save_run(&report.manifest)?;
        Ok(report)
    }

    /// Derive a ready step's work from its settled dependencies. Spend is read
    /// here, at launch: steps already running when the ceiling is crossed
    /// finish, and the run reports the overshoot.
    fn prepare(
        &self,
        step_id: &str,
        settled: &HashMap<String, Settled>,
        ceiling_micro_usd: Option<u64>,
    ) -> Work<'a> {
        let step = self.flow.step(step_id).expect("validated step");
        // A dependency blocks when it failed, or when it succeeded but a proof
        // refuted its claims. Work built on refuted work cannot lead to a
        // passing run, so running it would only spend time and money.
        let blocked = step
            .needs
            .iter()
            .filter_map(|need| {
                let receipt = &settled.get(need)?.receipt;
                if !receipt.identity.status.is_ok() {
                    Some(format!("{need} ({})", receipt.identity.status.label()))
                } else if receipt.claim_counts().refuted > 0 {
                    Some(format!("{need} (claims refuted)"))
                } else {
                    None
                }
            })
            .collect();
        let receipt_ids: HashMap<String, String> = settled
            .iter()
            .map(|(id, entry)| (id.clone(), entry.receipt.receipt_id.clone()))
            .collect();
        let deps = keying::dep_refs(step, &receipt_ids)
            .expect("dependencies settle before a step launches");
        let gate = ceiling_micro_usd.map(|ceiling_micro_usd| CostGate {
            ceiling_micro_usd,
            spent_micro_usd: settled.values().fold(0u64, |total, entry| {
                total.saturating_add(entry.spent_micro_usd())
            }),
        });
        Work {
            step,
            blocked,
            deps,
            gate,
        }
    }

    /// Produce one step's receipt: cancelled, reused, or freshly executed.
    ///
    /// A problem with this one step, such as a command that cannot start or an
    /// output that cannot be restored, becomes a receipt saying so, and the
    /// run still records a manifest with everything it spent. Only a store
    /// that cannot record receipts at all stops the run.
    fn settle(&self, work: Work<'_>, options: &RunOptions) -> Result<Settled> {
        let force = options.force;
        let Work {
            step,
            blocked,
            deps,
            gate,
        } = work;
        let keyed = self.keyed(step, self.facts(step), deps);
        // Facts about this run, such as why a cached receipt was passed over,
        // are notes. Only facts about the work go into a receipt.
        let mut notes = Vec::new();
        let mut attempts = Vec::new();
        let mut candidates = Vec::new();
        let mut vouched_by = Vec::new();

        let (receipt, cached) = if options.stop.is_set() {
            let why = "not run: flot was asked to stop".to_string();
            (self.not_run(keyed, why, options)?, false)
        } else if !blocked.is_empty() {
            let why = format!("not run: dependency {} did not hold", blocked.join(", "));
            (self.not_run(keyed, why, options)?, false)
        } else if let Some((known, restored)) = if force || step.always {
            None
        } else if self.drift(step, &keyed.facts.verify_inputs).is_some() {
            // A receipt proven against something else does not stand in.
            None
        } else if step.candidates.is_empty() {
            self.reuse(step, &keyed.step_key, &mut notes)
        } else {
            self.reuse_candidate(step, &keyed.deps, &mut notes)
        } {
            // Reuse costs nothing, so the ceiling never blocks a cache hit.
            notes.extend(restored);
            vouched_by = known.vouched_by;
            options.progress.emit(|| Event::StepCached {
                step: step.id.clone(),
                candidate: known.receipt.identity.candidate.clone(),
                claims: known.receipt.claim_counts(),
            });
            (known.receipt, true)
        } else if let Some(gate) =
            gate.filter(|gate| gate.spent_micro_usd >= gate.ceiling_micro_usd)
        {
            let why = format!(
                "not run: run cost ceiling of {} reached ({} spent by executed steps)",
                metrics::format_usd(gate.ceiling_micro_usd),
                metrics::format_usd(gate.spent_micro_usd)
            );
            (self.not_run(keyed, why, options)?, false)
        } else if step.candidates.is_empty() {
            let (receipt, earlier) = self.attempt(keyed, gate, options, &mut notes)?;
            attempts = earlier;
            (receipt, false)
        } else {
            let mut ran = self.compete(step, &keyed.deps, gate, options, &mut notes)?;
            if ran.is_empty() {
                let why = "not run: no candidate started".to_string();
                (self.not_run(keyed, why, options)?, false)
            } else {
                let kept = (0..ran.len())
                    .min_by_key(|&index| {
                        (
                            candidate_rank(&ran[index].receipt, ran[index].cost_micro_usd()),
                            index,
                        )
                    })
                    .expect("at least one candidate ran");
                let kept = ran.remove(kept);
                notes.extend(self.write_back(&kept.receipt));
                attempts = kept.attempts;
                candidates = ran;
                (kept.receipt, false)
            }
        };

        self.store.put_receipt(&receipt)?;
        // Only reusable work may answer for this key. A cancelled or failed
        // attempt must not displace a success recorded earlier. A reused
        // import is remembered with its signers again: remembered as this
        // host's work, it would outlive their trust and read as local work.
        if receipt.is_reusable() {
            self.store
                .remember(&receipt.identity.step_key, &receipt.receipt_id, &vouched_by)?;
        }
        Ok(Settled {
            receipt,
            cached,
            vouched_by,
            notes,
            attempts,
            candidates,
        })
    }

    /// The receipt of a step the run did not start, for `why`, reported as
    /// finished without executing.
    fn not_run(&self, keyed: Keyed<'_>, why: String, options: &RunOptions) -> Result<Receipt> {
        let receipt = self.unrun_receipt(Unrun::Cancelled, keyed, why)?;
        options
            .progress
            .emit(|| Event::finished(&receipt, 0, Duration::ZERO));
        Ok(receipt)
    }
}
