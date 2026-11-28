//! A recorded run as `flot show` prints it: each step, its attempts and
//! candidates, and each claim with what its proofs found.

use crate::core::digest;
use crate::core::receipt::{ClaimStatus, StepStatus};
use crate::service::RunDetail;

use super::super::terminal_safe;
use super::audit::{verdict_text, vouched_of, vouched_suffix};
use super::run::{render_limitations, render_refusals, render_spend};

pub(in super::super) fn render_detail(detail: &RunDetail) {
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
                if let Some(held) = &claim.held {
                    match (held.files.is_empty(), held.exit_code) {
                        (true, _) => say!("        held files: as the revision has them"),
                        (false, Some(0)) => {
                            say!("        with held files as the revision has them: passed")
                        }
                        (false, Some(code)) => {
                            say!("        with held files as the revision has them: exited {code}")
                        }
                        (false, None) => {
                            say!("        with held files as the revision has them: stopped")
                        }
                    }
                }
                if let Some(mutants) = &claim.mutants {
                    let mut tally = format!(
                        "        mutants: {} caught, {} survived",
                        mutants.caught,
                        mutants.survived.len()
                    );
                    if !mutants.allowed.is_empty() {
                        tally.push_str(&format!(", {} allowed", mutants.allowed.len()));
                    }
                    if mutants.untested > 0 {
                        tally.push_str(&format!(", {} untested", mutants.untested));
                    }
                    say!("{tally}");
                    // As an allow list entry names each, so a person who
                    // accepts one can copy it and give the reason.
                    let quoted = |text: &str| serde_json::to_string(text).unwrap_or_default();
                    for (label, survivors) in [
                        ("survived", &mutants.survived),
                        ("allowed", &mutants.allowed),
                    ] {
                        for survivor in survivors {
                            say!(
                                "          {label} at {}:{}: {{ file = {}, mutation = {}, code = {} }}",
                                survivor.file,
                                survivor.line,
                                quoted(&survivor.file),
                                quoted(&survivor.mutation),
                                quoted(&survivor.code)
                            );
                        }
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
        if step.claims.refuted > 0 {
            say!(
                "      proof output: flot log {} --proof {}",
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
