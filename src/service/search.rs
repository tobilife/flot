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

use crate::store::Store;

mod walk;

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
mod tests;
