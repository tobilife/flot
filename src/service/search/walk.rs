//! Walking one run for hits: every execution it made, and each receipt's
//! claims, limitations and outputs, until the hit limit is reached.

use super::{around, Hit, Place, Search};
use crate::core::digest;
use crate::core::receipt::{Receipt, RunManifest};

impl Search<'_> {
    /// Search one run. Returns false once the hit limit is reached.
    pub(super) fn run(&mut self, manifest: &RunManifest) -> bool {
        let place = Place {
            run_id: &manifest.run_id,
            step_id: None,
            candidate: None,
            attempt: None,
        };
        for limitation in &manifest.limitations {
            if !self.text(&place, "limitation", limitation) {
                return false;
            }
        }
        for step in &manifest.steps {
            if step.cached {
                continue;
            }
            // Each candidate's executions, oldest first; the kept one's are
            // the step's own attempts and receipt.
            let mut kept: Vec<&str> = step
                .attempts
                .iter()
                .map(|a| a.receipt_id.as_str())
                .collect();
            kept.push(&step.receipt_id);
            let mut executions = vec![(None, kept)];
            for candidate in &step.candidates {
                let mut tries: Vec<&str> = candidate
                    .attempts
                    .iter()
                    .map(|a| a.receipt_id.as_str())
                    .collect();
                tries.push(&candidate.receipt_id);
                executions.push((Some(candidate.name.as_str()), tries));
            }
            for (name, tries) in executions {
                let count = tries.len();
                for (index, receipt_id) in tries.into_iter().enumerate() {
                    let receipt = match self.store.find_receipt(receipt_id) {
                        Ok(receipt) if receipt.is_authentic() => receipt,
                        Ok(_) => {
                            self.report.unreadable.push(format!(
                                "{} step {}: receipt {} does not match its body",
                                manifest.run_id,
                                step.step_id,
                                digest::short(receipt_id)
                            ));
                            continue;
                        }
                        Err(error) => {
                            self.report.unreadable.push(format!(
                                "{} step {}: {error:#}",
                                manifest.run_id, step.step_id
                            ));
                            continue;
                        }
                    };
                    let place = Place {
                        step_id: Some(&step.step_id),
                        // The kept receipt names its own candidate.
                        candidate: name.or(receipt.identity.candidate.as_deref()),
                        attempt: (count > 1).then_some(index + 1),
                        ..place.clone()
                    };
                    if !self.receipt(&place, &receipt) {
                        return false;
                    }
                }
            }
        }
        true
    }

    fn receipt(&mut self, place: &Place, receipt: &Receipt) -> bool {
        let identity = &receipt.identity;
        for claim in &identity.claims {
            let said = match &claim.reason {
                Some(reason) => format!("{} [{}]: {reason}", claim.text, claim.status.label()),
                None => format!("{} [{}]", claim.text, claim.status.label()),
            };
            if !self.text(place, "claim", &said) {
                return false;
            }
        }
        for limitation in &identity.limitations {
            if !self.text(place, "limitation", limitation) {
                return false;
            }
        }
        let mut outputs = vec![
            ("stdout", identity.stdout_digest.as_str()),
            ("stderr", identity.stderr_digest.as_str()),
        ];
        outputs.extend(
            identity
                .claims
                .iter()
                .filter_map(|claim| claim.verify_output_digest.as_deref())
                .map(|digest| ("proof", digest)),
        );
        outputs.extend(identity.claims.iter().filter_map(|claim| {
            claim
                .before
                .as_ref()
                .map(|before| ("proof before the change", before.output_digest.as_str()))
        }));
        outputs.extend(identity.claims.iter().filter_map(|claim| {
            claim
                .held
                .as_ref()
                .and_then(|held| held.output_digest.as_deref())
                .map(|digest| ("proof with the held files", digest))
        }));
        outputs.extend(identity.claims.iter().filter_map(|claim| {
            claim
                .mutants
                .as_ref()
                .map(|mutants| ("mutation run", mutants.output_digest.as_str()))
        }));
        for (source, object) in outputs {
            let bytes = match self.store.read_bytes(object) {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.report.unreadable.push(format!(
                        "{} step {}: {source} {}: {error:#}",
                        place.run_id,
                        place.step_id.unwrap_or_default(),
                        digest::short(object)
                    ));
                    continue;
                }
            };
            let text = String::from_utf8_lossy(&bytes);
            for (number, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(self.needle)
                    && !self.hit(place, source, Some(number + 1), line)
                {
                    return false;
                }
            }
        }
        true
    }

    /// Match a recorded sentence, which may span lines, as one text.
    fn text(&mut self, place: &Place, source: &'static str, text: &str) -> bool {
        if !text.to_lowercase().contains(self.needle) {
            return true;
        }
        self.hit(place, source, None, text)
    }

    /// Record a hit. Returns false when the limit was already reached.
    fn hit(
        &mut self,
        place: &Place,
        source: &'static str,
        line: Option<usize>,
        text: &str,
    ) -> bool {
        if self.report.hits.len() >= self.limit {
            self.report.truncated = true;
            return false;
        }
        self.report.hits.push(Hit {
            run_id: place.run_id.to_string(),
            step_id: place.step_id.map(str::to_string),
            candidate: place.candidate.map(str::to_string),
            attempt: place.attempt,
            source,
            line,
            text: around(text, self.needle),
        });
        true
    }
}
