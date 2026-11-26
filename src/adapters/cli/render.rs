//! What commands print: lines for a person, safe on a terminal; the
//! Markdown report of `flot verify --markdown`; and pretty JSON.

use anyhow::Result;
use serde::Serialize;

use crate::core::attest::{self, Authorship, SignerCheck, VouchedStep, Voucher};
use crate::core::compare::RunDiff;
use crate::core::digest;
use crate::core::receipt::{ClaimStatus, Partial, RunManifest, StepStatus, Verdict};
use crate::service::audit::{Audit, Outcome};
use crate::service::{Plan, RunDetail, RunReport, SignReport};

use super::terminal_safe;

pub(super) fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", json_text(value)?);
    Ok(())
}

/// Pretty JSON that is also safe on a terminal. serde_json escapes only C0
/// controls, so DEL, C1 controls and bidi overrides would reach the terminal
/// raw. Such characters can only occur inside JSON strings, where a `\uXXXX`
/// escape means the same character, so escaping them keeps the document
/// byte-for-byte equivalent to a JSON reader.
fn json_text<T: Serialize>(value: &T) -> Result<String> {
    let text = serde_json::to_string_pretty(value)?;
    if !text.chars().any(needs_escape) {
        return Ok(text);
    }
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if needs_escape(c) {
            escaped.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            escaped.push(c);
        }
    }
    Ok(escaped)
}

/// Characters a terminal could act on, or that make text read differently
/// than it is. Newline and tab are layout, not control.
pub(super) fn needs_escape(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

pub(super) fn render_plan(plan: &Plan) {
    say!("flow {} ({})", plan.flow, digest::short(&plan.flow_digest));
    for entry in &plan.steps {
        let reuse = if entry.cached { "reuse" } else { "run" };
        let proof = match &entry.verified_by {
            Some(command) => format!("verify `{command}`"),
            None if entry.claims > 0 => "NO PROOF".to_string(),
            None => "no claims".to_string(),
        };
        say!(
            "  {reuse:<5} {:<20} inputs {:<3} claims {:<3} {proof}",
            entry.step_id,
            entry.inputs,
            entry.claims
        );
        if !entry.outputs.is_empty() {
            say!("        > produces {}", entry.outputs.join(", "));
        }
        for limitation in &entry.limitations {
            say!("        ! {limitation}");
        }
    }
}

pub(super) fn render_run(report: &RunReport) {
    for step in &report.steps {
        let mode = if step.cached {
            "cached"
        } else if step.status == StepStatus::Cancelled {
            "skipped"
        } else {
            "ran"
        };
        say!(
            "{:<8} {:<20} {:<9} {}ms  claims {}v/{}u/{}r  receipt {}{}{}{}",
            mode,
            step.step_id,
            step.status.label(),
            step.duration_ms,
            step.claims.verified,
            step.claims.unverified,
            step.claims.refuted,
            digest::short(&step.receipt_id),
            step.candidate
                .as_ref()
                .map(|name| format!("  candidate {name}"))
                .unwrap_or_default(),
            if step.attempts > 1 {
                format!("  attempt {}", step.attempts)
            } else {
                String::new()
            },
            vouched_of(&report.vouched, &step.step_id)
                .map(vouched_suffix)
                .unwrap_or_default()
        );
        for output in &step.outputs {
            say!("    > {}  {}", output.path, digest::short(&output.digest));
        }
        // A reused step's output was shown when it ran; `flot show` and
        // `flot cat` still have it.
        if step.cached {
            continue;
        }
        if !step.stdout_inline.trim().is_empty() {
            for line in step.stdout_inline.lines() {
                say!("    | {line}");
            }
        }
        if !step.stderr_inline.trim().is_empty() {
            for line in step.stderr_inline.lines() {
                say!("    ! {line}");
            }
        }
        if step.stdout_truncated || step.stderr_truncated {
            say!(
                "    complete output: flot log {}{} {}",
                step.step_id,
                if step.stdout_truncated {
                    ""
                } else {
                    " --stderr"
                },
                report.manifest.run_id
            );
        }
    }
    let manifest = &report.manifest;
    say!();
    say!(
        "{} {}  root {}  {}ms",
        Outcome::of_run(manifest).label().to_uppercase(),
        manifest.run_id,
        digest::short(&manifest.root_digest),
        manifest.duration_ms
    );
    render_spend(manifest);
    render_limitations(manifest);
    render_refusals(manifest);
}

fn render_spend(manifest: &RunManifest) {
    let spend = &manifest.spend;
    if spend.reporting_steps == 0
        && spend.reused_cost_micro_usd == 0
        && manifest.max_cost_micro_usd.is_none()
    {
        return;
    }
    let mut line = format!(
        "spent {} (in {} / out {} tokens, {} reporting step(s)",
        crate::core::metrics::format_usd(spend.cost_micro_usd),
        spend.input_tokens,
        spend.output_tokens,
        spend.reporting_steps
    );
    if spend.silent_steps > 0 {
        line.push_str(&format!(", {} silent", spend.silent_steps));
    }
    if spend.unpriced_steps > 0 {
        line.push_str(&format!(", {} without cost", spend.unpriced_steps));
    }
    line.push(')');
    if spend.reused_cost_micro_usd > 0 {
        line.push_str(&format!(
            "  reused {} of earlier spend",
            crate::core::metrics::format_usd(spend.reused_cost_micro_usd)
        ));
    }
    if let Some(ceiling) = manifest.max_cost_micro_usd {
        line.push_str(&format!(
            "  ceiling {}",
            crate::core::metrics::format_usd(ceiling)
        ));
    }
    say!("{line}");
}

/// The run's limitations, with one line for a message several steps share
/// ("a, b: step declares no inputs; …") rather than one line per step.
pub(super) fn render_limitations(manifest: &RunManifest) {
    if manifest.limitations.is_empty() {
        return;
    }
    let mut lines: Vec<(Vec<&str>, &str)> = Vec::new();
    for limitation in &manifest.limitations {
        let by_step = limitation
            .split_once(": ")
            .filter(|(step, _)| manifest.steps.iter().any(|entry| entry.step_id == *step));
        match by_step {
            Some((step, message)) => {
                match lines
                    .iter_mut()
                    .find(|(steps, seen)| !steps.is_empty() && *seen == message)
                {
                    Some((steps, _)) => steps.push(step),
                    None => lines.push((vec![step], message)),
                }
            }
            None => lines.push((Vec::new(), limitation)),
        }
    }
    say!("limitations:");
    for (steps, message) in lines {
        if steps.is_empty() {
            say!("  - {message}");
        } else {
            say!("  - {}: {message}", steps.join(", "));
        }
    }
}

/// At most this many refusals are listed one by one.
const REFUSALS_SHOWN: usize = 20;

/// What the guard refused agents while the run's evidence was being made.
fn render_refusals(manifest: &RunManifest) {
    if manifest.refusals.is_empty() {
        return;
    }
    say!("refused by the guard since the previous run:");
    for refusal in manifest.refusals.iter().take(REFUSALS_SHOWN) {
        say!("  - {}: {}", refusal.tool, refusal.reasons.join("; "));
    }
    if manifest.refusals.len() > REFUSALS_SHOWN {
        say!("  - and {} more", manifest.refusals.len() - REFUSALS_SHOWN);
    }
}

pub(super) fn render_detail(detail: &RunDetail) {
    let manifest = &detail.manifest;
    say!(
        "run {}  flow {}  root {}{}",
        manifest.run_id,
        manifest.flow,
        digest::short(&manifest.root_digest),
        if detail.imported { "  (imported)" } else { "" }
    );
    for step in &manifest.steps {
        say!(
            "  {:<20} {:<9} {}  receipt {}{}",
            step.step_id,
            step.status.label(),
            if step.cached { "cached" } else { "ran   " },
            digest::short(&step.receipt_id),
            vouched_of(&detail.vouched, &step.step_id)
                .map(vouched_suffix)
                .unwrap_or_default()
        );
        for (number, attempt) in step.attempts.iter().enumerate() {
            say!(
                "      attempt {}  receipt {}  did not hold",
                number + 1,
                digest::short(&attempt.receipt_id)
            );
        }
        for candidate in &step.candidates {
            say!(
                "      candidate {}  receipt {}  not kept",
                candidate.name,
                digest::short(&candidate.receipt_id)
            );
        }
        if let Some(claims) = detail.step_claims.get(&step.step_id) {
            for claim in claims {
                let mark = match claim.status {
                    ClaimStatus::Verified => "+",
                    ClaimStatus::Unverified => "?",
                    ClaimStatus::Refuted => "x",
                };
                say!("      {mark} [{}] {}", claim.status.label(), claim.text);
                if let Some(reason) = &claim.reason {
                    say!("        reason: {reason}");
                }
                if let Some(before) = &claim.before {
                    let commit = &before.commit[..before.commit.len().min(12)];
                    match before.exit_code {
                        Some(0) => say!("        before the change ({commit}): passed"),
                        Some(code) => say!("        before the change ({commit}): exited {code}"),
                        None => say!("        before the change ({commit}): stopped"),
                    }
                }
            }
        }
        if matches!(step.status, StepStatus::Failed | StepStatus::Timeout) {
            say!(
                "      output: flot log {} {}",
                step.step_id,
                manifest.run_id
            );
        }
    }
    say!(
        "verdict {}  claims {}v/{}u/{}r  concurrency {}",
        verdict_text(manifest.verdict, manifest.partial),
        manifest.claims.verified,
        manifest.claims.unverified,
        manifest.claims.refuted,
        manifest.max_parallel
    );
    render_spend(manifest);
    render_limitations(manifest);
    render_refusals(manifest);
}

/// A recorded verdict as a report states it: a run of some of the flow's
/// steps reads `partial` when they passed, and names its selection.
fn verdict_text(verdict: Verdict, partial: Option<Partial>) -> String {
    match partial {
        None => verdict.label().to_string(),
        Some(partial) if verdict == Verdict::Passed => format!(
            "partial ({} of {} steps selected, and they passed)",
            partial.selected, partial.of
        ),
        Some(partial) => format!(
            "{} ({} of {} steps selected)",
            verdict.label(),
            partial.selected,
            partial.of
        ),
    }
}

pub(super) fn render_audit(audit: &Audit) {
    say!(
        "run {}  receipts checked {}  root {}",
        audit.run_id,
        audit.checked_receipts,
        digest::short(&audit.root_digest)
    );
    if audit.intact {
        say!("integrity: intact");
    } else {
        say!("integrity: BROKEN");
        for finding in &audit.findings {
            say!("  - {finding}");
        }
    }
    say!("verdict {}", verdict_text(audit.verdict, audit.partial));
    if let Some(anchor) = audit.anchor() {
        if anchor.holds() {
            say!("proofs: as at {}", anchor.baseline);
        } else {
            say!("proofs: NOT as at {}", anchor.baseline);
            for drift in &anchor.drifted {
                say!("  - {}: {}", drift.step_id, drift.reasons.join(", "));
            }
            if !anchor.missing.is_empty() {
                say!(
                    "  - claims there that this run does not make: {}",
                    anchor.missing.join(", ")
                );
            }
        }
        if !anchor.unread.is_empty() {
            say!("changes no step read: {}", listed(&anchor.unread));
        }
    }
    render_authorship(audit.authorship, &audit.signers);
    if !audit.vouched.is_empty() {
        say!("reused from signed runs: {} step(s)", audit.vouched.len());
        for step in &audit.vouched {
            say!(
                "  - {}: vouched for by {}",
                step.step_id,
                named(&step.vouched_by)
            );
        }
    }
}

/// The signers a reused step rests on, as a report names them.
fn named(vouched_by: &[Voucher]) -> String {
    vouched_by
        .iter()
        .map(Voucher::label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a step line ends with when its receipt came from an imported run:
/// nothing for work this host did.
fn vouched_suffix(vouched_by: &[Voucher]) -> String {
    if vouched_by.is_empty() {
        String::new()
    } else {
        format!("  vouched for by {}", named(vouched_by))
    }
}

/// Who vouched for `step_id`, when the run reused it from an imported run.
fn vouched_of<'v>(vouched: &'v [VouchedStep], step_id: &str) -> Option<&'v [Voucher]> {
    vouched
        .iter()
        .find(|step| step.step_id == step_id)
        .map(|step| step.vouched_by.as_slice())
}

/// Up to 20 paths, and how many more there are.
fn listed(paths: &[String]) -> String {
    const SHOWN: usize = 20;
    let mut text = paths[..paths.len().min(SHOWN)].join(", ");
    if paths.len() > SHOWN {
        text.push_str(&format!(" and {} more", paths.len() - SHOWN));
    }
    text
}

/// `text` as Markdown that renders as the same text: a claim or a limitation
/// may come from an agent, and must not add links, images, markup or rows.
fn md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.'
            | '!' | '|' | '~' | '>' | '=' => {
                out.push('\\');
                out.push(character);
            }
            '<' => out.push_str("&lt;"),
            '&' => out.push_str("&amp;"),
            '\n' | '\r' => out.push(' '),
            other if other.is_control() => {}
            other => out.push(other),
        }
    }
    out
}

/// A run and the check `flot verify` made of it, for a pull request comment
/// or a CI job summary.
pub(super) fn markdown_report(audit: &Audit, detail: &RunDetail) -> String {
    use std::fmt::Write;
    let manifest = &detail.manifest;
    // The same judgment as the exit code of `flot verify`.
    let headline = match (audit.result(), audit.anchor(), audit.partial) {
        (Outcome::Broken, _, _) => "evidence broken".to_string(),
        (Outcome::Unverified, Some(anchor), _)
            if !anchor.holds() && manifest.verdict == Verdict::Passed =>
        {
            format!("unverified, proofs differ from {}", md(&anchor.baseline))
        }
        (Outcome::Partial, _, Some(partial)) => format!(
            "partial, {} of {} steps selected",
            partial.selected, partial.of
        ),
        (result, _, _) => result.label().to_string(),
    };
    let mut out = String::new();
    let _ = writeln!(out, "### flot: {headline}\n");
    let _ = writeln!(
        out,
        "Run `{}` of flow {}, root `{}`{}; recorded verdict {}.\n",
        audit.run_id,
        md(&manifest.flow),
        digest::short(&audit.root_digest),
        if detail.imported { ", imported" } else { "" },
        manifest.verdict.label()
    );
    let _ = writeln!(
        out,
        "- Integrity: {}",
        if audit.intact { "intact" } else { "**broken**" }
    );
    for finding in &audit.findings {
        let _ = writeln!(out, "  - {}", md(finding));
    }
    if let Some(anchor) = audit.anchor() {
        if anchor.holds() {
            let _ = writeln!(out, "- Proofs: as at {}", md(&anchor.baseline));
        } else {
            let _ = writeln!(out, "- Proofs: **not as at {}**", md(&anchor.baseline));
            for drift in &anchor.drifted {
                let _ = writeln!(
                    out,
                    "  - `{}`: {}",
                    drift.step_id,
                    md(&drift.reasons.join(", "))
                );
            }
            if !anchor.missing.is_empty() {
                let _ = writeln!(
                    out,
                    "  - claims there that this run does not make: {}",
                    md(&anchor.missing.join(", "))
                );
            }
        }
        if !anchor.unread.is_empty() {
            let _ = writeln!(
                out,
                "- Changes no step read: {}",
                md(&listed(&anchor.unread))
            );
        }
    }
    let _ = writeln!(out, "- Authorship: {}", audit.authorship.label());
    if !audit.vouched.is_empty() {
        // Each signer once, in the order the steps name them.
        let mut signers: Vec<String> = Vec::new();
        for signer in audit.vouched.iter().flat_map(|step| &step.vouched_by) {
            let label = signer.label();
            if !signers.contains(&label) {
                signers.push(label);
            }
        }
        let _ = writeln!(
            out,
            "- Reused from signed runs: {} step(s), vouched for by {}",
            audit.vouched.len(),
            md(&signers.join(", "))
        );
    }
    let counts = manifest.claims;
    let _ = writeln!(
        out,
        "- Claims: {} verified, {} unverified, {} refuted",
        counts.verified, counts.unverified, counts.refuted
    );
    if manifest.spend.reporting_steps > 0 {
        let _ = writeln!(
            out,
            "- Spend: {}",
            crate::core::metrics::format_usd(manifest.spend.cost_micro_usd)
        );
    }
    if !manifest.refusals.is_empty() {
        let _ = writeln!(
            out,
            "- Refused by the guard since the previous run: {}",
            manifest.refusals.len()
        );
        for refusal in manifest.refusals.iter().take(REFUSALS_SHOWN) {
            let _ = writeln!(
                out,
                "  - {}: {}",
                md(&refusal.tool),
                md(&refusal.reasons.join("; "))
            );
        }
        if manifest.refusals.len() > REFUSALS_SHOWN {
            let _ = writeln!(
                out,
                "  - and {} more",
                manifest.refusals.len() - REFUSALS_SHOWN
            );
        }
    }
    let _ = writeln!(out, "\n| Step | Status | Claim | |\n|---|---|---|---|");
    for step in &manifest.steps {
        let status = format!(
            "{}{}{}",
            step.status.label(),
            if step.cached { ", reused" } else { "" },
            vouched_of(&audit.vouched, &step.step_id)
                .map(|vouched_by| format!(", vouched for by {}", md(&named(vouched_by))))
                .unwrap_or_default()
        );
        let claims = detail.step_claims.get(&step.step_id);
        match claims.filter(|claims| !claims.is_empty()) {
            None => {
                let _ = writeln!(out, "| `{}` | {status} | | |", step.step_id);
            }
            Some(claims) => {
                for (index, claim) in claims.iter().enumerate() {
                    let step_cell = if index == 0 {
                        format!("`{}`", step.step_id)
                    } else {
                        String::new()
                    };
                    let status_cell = if index == 0 { status.as_str() } else { "" };
                    let text = match &claim.reason {
                        Some(reason) => format!("{} ({})", md(&claim.text), md(reason)),
                        None => md(&claim.text),
                    };
                    let _ = writeln!(
                        out,
                        "| {step_cell} | {status_cell} | {text} | {} |",
                        claim.status.label()
                    );
                }
            }
        }
    }
    if !manifest.limitations.is_empty() {
        let _ = writeln!(out, "\n<details><summary>Limitations</summary>\n");
        for limitation in &manifest.limitations {
            let _ = writeln!(out, "- {}", md(limitation));
        }
        let _ = writeln!(out, "\n</details>");
    }
    out
}

pub(super) fn render_signed(signed: &SignReport) {
    say!(
        "{} run {}  root {}  by {}",
        if signed.added {
            "signed"
        } else {
            "already signed"
        },
        signed.run_id,
        digest::short(&signed.root_digest),
        signed.signer
    );
}

pub(super) fn render_authorship(authorship: Authorship, signers: &[SignerCheck]) {
    say!("authorship: {}", authorship.label());
    for signer in signers {
        match (&signer.problem, &signer.trusted_as, signer.this_host) {
            (Some(problem), _, _) => say!("  x {}  {problem}", attest::fingerprint(&signer.key)),
            (None, Some(name), _) => say!(
                "  + {}  trusted as {name}",
                attest::fingerprint(&signer.key)
            ),
            (None, None, true) => say!(
                "  ~ {}  this host's key, not on the trust list",
                attest::fingerprint(&signer.key)
            ),
            (None, None, false) => say!(
                "  ? {}  not on the trust list",
                attest::fingerprint(&signer.key)
            ),
        }
    }
}

pub(super) fn render_diff(diff: &RunDiff) {
    say!("left  {}  {}", diff.left, diff.left_verdict.label());
    say!("right {}  {}", diff.right, diff.right_verdict.label());
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
    }
}
