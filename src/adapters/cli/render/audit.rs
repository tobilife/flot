//! What `flot verify` and `flot sign` print: integrity, the proofs against a
//! revision, authorship, and who vouched for the steps a run reused.

use crate::core::attest::{self, Authorship, SignerCheck, VouchedStep, Voucher};
use crate::core::digest;
use crate::core::receipt::{Partial, Verdict};
use crate::service::audit::Audit;
use crate::service::SignReport;

use super::super::terminal_safe;

/// A recorded verdict as a report states it: a run of some of the flow's
/// steps reads `partial` when they passed, and names its selection.
pub(super) fn verdict_text(verdict: Verdict, partial: Option<Partial>) -> String {
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

pub(in super::super) fn render_audit(audit: &Audit) {
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
            if audit.only_proofs_differ() {
                say!("only the proofs differ: {ONLY_PROOFS_DIFFER}");
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
pub(super) fn named(vouched_by: &[Voucher]) -> String {
    vouched_by
        .iter()
        .map(Voucher::label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a step line ends with when its receipt came from an imported run:
/// nothing for work this host did.
pub(super) fn vouched_suffix(vouched_by: &[Voucher]) -> String {
    if vouched_by.is_empty() {
        String::new()
    } else {
        format!("  vouched for by {}", named(vouched_by))
    }
}

/// Who vouched for `step_id`, when the run reused it from an imported run.
pub(super) fn vouched_of<'v>(vouched: &'v [VouchedStep], step_id: &str) -> Option<&'v [Voucher]> {
    vouched
        .iter()
        .find(|step| step.step_id == step_id)
        .map(|step| step.vouched_by.as_slice())
}

/// Up to 20 paths, and how many more there are.
pub(super) fn listed(paths: &[String]) -> String {
    const SHOWN: usize = 20;
    let mut text = paths[..paths.len().min(SHOWN)].join(", ");
    if paths.len() > SHOWN {
        text.push_str(&format!(" and {} more", paths.len() - SHOWN));
    }
    text
}

/// What a run kept from passing only by the change to its proofs asks of the
/// reader.
pub(super) const ONLY_PROOFS_DIFFER: &str =
    "no step failed, and every claim left unverified is one of a \
    step whose proof changed; a person reviews that change, which no run proves";

pub(in super::super) fn render_signed(signed: &SignReport) {
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

#[cfg(test)]
mod tests;
