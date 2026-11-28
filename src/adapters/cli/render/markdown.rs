//! The Markdown report of `flot verify --markdown`, for a pull request
//! comment or a CI job summary.

use crate::core::digest;
use crate::core::receipt::Verdict;
use crate::service::audit::{Audit, Outcome};
use crate::service::RunDetail;

use super::audit::{listed, named, vouched_of, ONLY_PROOFS_DIFFER};
use super::run::REFUSALS_SHOWN;

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
pub(in super::super) fn markdown_report(audit: &Audit, detail: &RunDetail) -> String {
    use std::fmt::Write;
    let manifest = &detail.manifest;
    // The same judgment as the exit code of `flot verify`.
    let headline = match (audit.result(), audit.anchor(), audit.partial) {
        (Outcome::Broken, _, _) => "evidence broken".to_string(),
        (Outcome::Unverified, Some(anchor), _) if audit.only_proofs_differ() => {
            format!(
                "unverified, only the proofs differ from {}",
                md(&anchor.baseline)
            )
        }
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
    if audit.only_proofs_differ() {
        let _ = writeln!(out, "Only the proofs differ: {}\n", md(ONLY_PROOFS_DIFFER));
    }
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
