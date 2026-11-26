//! Running a step again while its attempts last, telling each next attempt
//! why the last one did not hold.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};

use crate::core::metrics;
use crate::core::preview::stream_preview;
use crate::core::receipt::Receipt;
use crate::engine::exec;

use super::execute::Unrun;
use super::{CostGate, Keyed, Runner, METRICS_SEQUENCE};

impl Runner<'_> {
    /// Execute a step until it holds or its attempts run out. An attempt that
    /// did not hold is kept, and the next one is told why in
    /// `$FLOT_FEEDBACK`. A later attempt sees the workspace the earlier one
    /// left, so its facts and key are read again.
    ///
    /// No further attempt starts once flot is asked to stop, once the run's
    /// cost ceiling is reached, or after a command that could not start at
    /// all, which another attempt would not change.
    pub(super) fn attempt(
        &self,
        mut keyed: Keyed<'_>,
        gate: Option<CostGate>,
        stop: &exec::StopSignal,
        notes: &mut Vec<String>,
    ) -> Result<(Receipt, Vec<Receipt>)> {
        let step = keyed.step;
        let allowed = step.attempts();
        let mut earlier: Vec<Receipt> = Vec::new();
        // The workspace before the first attempt: each attempt's receipt
        // answers for what every attempt so far changed.
        let mut since = None;
        loop {
            let number = earlier.len() as u32 + 1;
            let mut extra = BTreeMap::new();
            if allowed > 1 {
                extra.insert("FLOT_ATTEMPT".to_string(), number.to_string());
                extra.insert("FLOT_ATTEMPTS".to_string(), allowed.to_string());
            }
            let feedback = match earlier.last() {
                Some(previous) => Some(self.write_feedback(previous, number - 1, allowed)?),
                None => None,
            };
            if let Some(path) = &feedback {
                extra.insert(
                    "FLOT_FEEDBACK".to_string(),
                    path.to_string_lossy().to_string(),
                );
            }
            let executed = self.execute(keyed.clone(), &extra, stop, &mut since);
            if let Some(path) = &feedback {
                let _ = std::fs::remove_file(path);
            }
            let receipt = match executed {
                Ok(receipt) => receipt,
                Err(error) => {
                    let why = format!("could not run: {error:#}");
                    let receipt = self.unrun_receipt(Unrun::Failed, keyed, why)?;
                    return Ok((receipt, earlier));
                }
            };
            if receipt.is_reusable() || number >= allowed || stop.is_set() {
                return Ok((receipt, earlier));
            }
            if let Some(gate) = gate {
                let spent = earlier
                    .iter()
                    .chain(std::iter::once(&receipt))
                    .filter_map(|receipt| receipt.observed.metrics?.cost_micro_usd)
                    .fold(gate.spent_micro_usd, u64::saturating_add);
                if spent >= gate.ceiling_micro_usd {
                    notes.push(format!(
                        "attempt {} of {allowed} not run: run cost ceiling of {} reached ({} spent by executed steps)",
                        number + 1,
                        metrics::format_usd(gate.ceiling_micro_usd),
                        metrics::format_usd(spent)
                    ));
                    return Ok((receipt, earlier));
                }
            }
            self.store.put_receipt(&receipt)?;
            earlier.push(receipt);
            keyed = self.keyed(step, self.facts(step), keyed.deps);
        }
    }

    /// Tell the next attempt why this one did not hold: how it ended, each
    /// claim's verdict, its limitations, and the end of what its proof and
    /// its command printed, all read back from the recorded evidence.
    fn write_feedback(&self, receipt: &Receipt, number: u32, allowed: u32) -> Result<PathBuf> {
        /// Room for each stream, kept from its end, where failures are told.
        const ROOM: usize = 8 * 1024;
        let identity = &receipt.identity;
        let mut text = format!(
            "flot: attempt {number} of {allowed} at step `{}` did not hold.\n\n",
            identity.step_id
        );
        text.push_str(&format!("status: {}", identity.status.label()));
        if let Some(code) = identity.exit_code {
            text.push_str(&format!(" (exit {code})"));
        }
        text.push('\n');
        if !identity.claims.is_empty() {
            text.push_str("claims:\n");
            for claim in &identity.claims {
                text.push_str(&format!("  - {}: {}\n", claim.status.label(), claim.text));
                if let Some(reason) = &claim.reason {
                    text.push_str(&format!("    {reason}\n"));
                }
            }
        }
        if !identity.limitations.is_empty() {
            text.push_str("limitations:\n");
            for limitation in &identity.limitations {
                text.push_str(&format!("  - {limitation}\n"));
            }
        }
        let mut stream = |title: String, digest: &str| -> Result<()> {
            let bytes = self.store.read_bytes(digest)?;
            if bytes.is_empty() {
                return Ok(());
            }
            let (preview, _) = stream_preview(&bytes, ROOM);
            text.push_str(&format!("\n--- {title} ---\n{preview}"));
            if !preview.ends_with('\n') {
                text.push('\n');
            }
            Ok(())
        };
        let mut proofs: Vec<(&str, &str)> = identity
            .claims
            .iter()
            .filter_map(|claim| {
                Some((
                    claim.verify_command.as_deref()?,
                    claim.verify_output_digest.as_deref()?,
                ))
            })
            .collect();
        proofs.dedup();
        for (command, digest) in proofs {
            stream(format!("output of the proof `{command}`"), digest)?;
        }
        stream("stdout".into(), &identity.stdout_digest)?;
        stream("stderr".into(), &identity.stderr_digest)?;
        let path = self.store.root().join("tmp").join(format!(
            "feedback-{}-{}.txt",
            std::process::id(),
            METRICS_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.parent().expect("tmp has a parent"))?;
        std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(path)
    }
}
