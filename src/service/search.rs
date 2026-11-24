//! Search recorded runs for text.
//!
//! The search is a literal, case-insensitive match over what runs recorded:
//! each step's complete output, its proofs' output, its claims and their
//! reasons, its limitations, and the run's own limitations. It is exact and
//! repeatable: the same store and query give the same hits in the same order.
//!
//! A step a run reused is not searched again in that run; its hits belong to
//! the run that executed it. Every execution a run made is searched: earlier
//! attempts and candidates it did not keep, as well as the receipt it kept.

use anyhow::Result;
use serde::Serialize;

use crate::core::digest;
use crate::core::receipt::{Receipt, RunManifest};
use crate::store::Store;

/// The longest line a hit carries; longer lines are cut around the match.
const LINE_CHARS: usize = 240;

#[derive(Debug, Clone, Default, Serialize)]
pub struct SearchReport {
    pub query: String,
    /// Runs searched, newest first.
    pub runs_searched: usize,
    pub hits: Vec<Hit>,
    /// More hits exist than `limit` allowed.
    pub truncated: bool,
    /// Runs or receipts that could not be read; they were not searched.
    pub unreadable: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Hit {
    pub run_id: String,
    /// `None` for a limitation of the run itself.
    pub step_id: Option<String>,
    /// The candidate whose execution this is, when the step names candidates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
    /// The attempt, numbered from 1, when the execution took more than one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<usize>,
    /// `stdout`, `stderr`, `proof`, `claim` or `limitation`.
    pub source: &'static str,
    /// Line number within an output, from 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub text: String,
}

/// Search the newest `runs` recorded runs; stop after `limit` hits.
pub fn search(store: &Store, query: &str, runs: usize, limit: usize) -> Result<SearchReport> {
    let mut report = SearchReport {
        query: query.to_string(),
        ..SearchReport::default()
    };
    let needle = query.to_lowercase();
    if needle.is_empty() {
        anyhow::bail!("the search query is empty");
    }
    let mut search = Search {
        store,
        needle: &needle,
        limit,
        report: &mut report,
    };
    for run_id in store.list_runs()?.into_iter().take(runs) {
        match store.resolve_run(&run_id) {
            Ok(manifest) => {
                search.report.runs_searched += 1;
                if !search.run(&manifest) {
                    break;
                }
            }
            Err(error) => search
                .report
                .unreadable
                .push(format!("{run_id}: {error:#}")),
        }
    }
    Ok(report)
}

struct Search<'a> {
    store: &'a Store,
    needle: &'a str,
    limit: usize,
    report: &'a mut SearchReport,
}

/// Where in a run a hit was found.
#[derive(Clone)]
struct Place<'a> {
    run_id: &'a str,
    step_id: Option<&'a str>,
    candidate: Option<&'a str>,
    attempt: Option<usize>,
}

impl Search<'_> {
    /// Search one run. Returns false once the hit limit is reached.
    fn run(&mut self, manifest: &RunManifest) -> bool {
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

/// `text` on one line, cut to [`LINE_CHARS`] characters around the first
/// match when longer, with each cut marked.
fn around(text: &str, needle: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= LINE_CHARS {
        return flat;
    }
    // Lowercasing can change a string's length, so find the match by
    // character position in a lowercased copy of each prefix.
    let lower: Vec<char> = flat.to_lowercase().chars().collect();
    let at = if lower.len() == chars.len() {
        let needle: Vec<char> = needle.chars().collect();
        lower
            .windows(needle.len().max(1))
            .position(|window| window == needle.as_slice())
            .unwrap_or(0)
    } else {
        0
    };
    let start = at.saturating_sub(LINE_CHARS / 3);
    let end = (start + LINE_CHARS).min(chars.len());
    let start = end.saturating_sub(LINE_CHARS);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_line_is_cut_around_the_match() {
        let line = format!("{}NEEDLE{}", "a".repeat(500), "b".repeat(500));
        let cut = around(&line, "needle");
        assert!(cut.contains("NEEDLE"), "{cut}");
        assert!(cut.starts_with('…') && cut.ends_with('…'), "{cut}");
        assert_eq!(cut.chars().count(), LINE_CHARS + 2);
        assert_eq!(around("short NEEDLE", "needle"), "short NEEDLE");
        assert_eq!(around("two\nlines", "two"), "two lines");
    }
}
