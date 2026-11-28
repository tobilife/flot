//! What reading across runs prints: two runs compared, the steps and proofs
//! that changed their result, search hits, and the list of runs.

use crate::core::compare::RunDiff;
use crate::core::receipt::{Partial, Verdict};
use crate::service::flaky::FlakyReport;
use crate::service::search::SearchReport;
use crate::service::RunList;

use super::super::terminal_safe;

pub(in super::super) fn render_diff(diff: &RunDiff) {
    let verdict = |verdict: Verdict, partial: Option<Partial>| match partial {
        Some(partial) => format!(
            "{}, partial: {} of {} steps",
            verdict.label(),
            partial.selected,
            partial.of
        ),
        None => verdict.label().to_string(),
    };
    say!(
        "left  {}  {}",
        diff.left,
        verdict(diff.left_verdict, diff.left_partial)
    );
    say!(
        "right {}  {}",
        diff.right,
        verdict(diff.right_verdict, diff.right_partial)
    );
    if !diff.same_flow {
        say!("flow definition differs between the two runs");
    }
    if diff.identical {
        say!("identical: same root digest, step for step");
        return;
    }
    for step in &diff.steps {
        say!(
            "  {:<20} {:<24} {} -> {}",
            step.step_id,
            step.change,
            step.left.clone().unwrap_or_else(|| "-".into()),
            step.right.clone().unwrap_or_else(|| "-".into())
        );
        for reason in &step.reasons {
            say!("        {reason}");
        }
    }
}

pub(in super::super) fn render_flaky(report: &FlakyReport) {
    for found in report.steps.iter().chain(&report.claims) {
        match &found.claim {
            None => say!(
                "step `{}` (key {}) on the same inputs:",
                found.step_id,
                found.key
            ),
            Some(claim) => say!(
                "proof of \"{}\" (step `{}`, key {}) over the same files:",
                claim,
                found.step_id,
                found.key
            ),
        }
        for outcome in &found.outcomes {
            let count = outcome.runs.len();
            say!(
                "  {:<9} in {count} run{}, newest {}",
                outcome.outcome,
                if count == 1 { "" } else { "s" },
                outcome.runs[0]
            );
        }
    }
    for unreadable in &report.unreadable {
        say!("unreadable  {unreadable}");
    }
    say!(
        "{} step(s) and {} proof(s) gave different results on the same inputs in {} run(s) read",
        report.steps.len(),
        report.claims.len(),
        report.runs_read
    );
}

pub(in super::super) fn render_search(report: &SearchReport) {
    for hit in &report.hits {
        let mut place = hit.step_id.clone().unwrap_or_else(|| "(run)".into());
        if let Some(candidate) = &hit.candidate {
            place.push_str(&format!(" candidate {candidate}"));
        }
        if let Some(attempt) = hit.attempt {
            place.push_str(&format!(" attempt {attempt}"));
        }
        let source = match hit.line {
            Some(line) => format!("{}:{line}", hit.source),
            None => hit.source.to_string(),
        };
        say!("{}  {place}  {source}  {}", hit.run_id, hit.text);
    }
    say!(
        "{} hit(s) in {} run(s) searched{}",
        report.hits.len(),
        report.runs_searched,
        if report.truncated {
            "; more hits exist, raise --limit"
        } else {
            ""
        }
    );
    for unreadable in &report.unreadable {
        say!("unreadable  {unreadable}");
    }
}

pub(in super::super) fn render_runs(list: &RunList) {
    if list.runs.is_empty() && list.unreadable.is_empty() {
        say!("no runs recorded");
    } else {
        for run in &list.runs {
            say!(
                "{}  {:<10} {} steps  claims {}v/{}u/{}r{}",
                run.run_id,
                run.verdict.label(),
                run.steps,
                run.claims.verified,
                run.claims.unverified,
                run.claims.refuted,
                if run.imported { "  (imported)" } else { "" }
            );
        }
        for unreadable in &list.unreadable {
            say!("unreadable  {unreadable}");
        }
    }
}
