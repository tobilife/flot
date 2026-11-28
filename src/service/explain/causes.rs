//! What one receipt says kept its step from holding, read from what it
//! records rather than from its wording, each with what to read or do next.

use crate::core::receipt::{ClaimRecord, ClaimStatus, Receipt, StepStatus};
use crate::engine::runner::UNCONFINED;

use super::{Cause, Kind};

/// How many surviving mutants a cause names before it counts the rest.
const NAMED_SURVIVORS: usize = 3;

/// The causes a step's receipt records, in the order they arose: how the
/// step ended, then each kind of proof that did not hold, once each.
/// `run` is the run selector the commands it suggests name, empty for the
/// latest run.
pub(super) fn causes(receipt: &Receipt, run: &str) -> Vec<Cause> {
    let identity = &receipt.identity;
    let step = identity.step_id.as_str();
    let log = |flag: &str| format!("flot log {step}{flag}{run}");
    let noted = |prefix: &str| {
        identity
            .limitations
            .iter()
            .find(|limitation| limitation.starts_with(prefix))
            .cloned()
    };
    let ended = match identity.status {
        StepStatus::Ok => match identity.exit_code {
            Some(code) if code != 0 => Some(Cause::new(
                Kind::Failed,
                format!("its command exited {code}; allow_failure kept the run going, but proves nothing"),
                log(" --stderr"),
            )),
            _ => None,
        },
        StepStatus::Cancelled => Some(Cause::new(
            Kind::NotRun,
            noted("not run: ")
                .and_then(|why| why.strip_prefix("not run: ").map(str::to_string))
                .unwrap_or_else(|| "the run did not start it".into()),
            format!("flot explain{run}"),
        )),
        StepStatus::Failed if said(&identity.claims, UNCONFINED) => Some(Cause::new(
            Kind::Sandbox,
            noted("could not run:").unwrap_or_else(|| UNCONFINED.into()),
            "flot doctor",
        )),
        StepStatus::Failed => Some(Cause::new(
            Kind::Failed,
            match identity.exit_code {
                Some(code) => format!("its command exited {code}"),
                None => identity
                    .limitations
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "it could not run".into()),
            },
            log(" --stderr"),
        )),
        StepStatus::Timeout => Some(Cause::new(
            Kind::TimedOut,
            noted("stopped at its").unwrap_or_else(|| "it was stopped at its timeout".into()),
            log(" --stderr"),
        )),
    };
    let mut found: Vec<Cause> = ended.into_iter().collect();
    // A step that did not succeed proved nothing; that is its cause.
    if !found.is_empty() {
        return found;
    }
    for claim in identity
        .claims
        .iter()
        .filter(|claim| claim.status != ClaimStatus::Verified)
    {
        let cause = claim_cause(claim, &log, run);
        if !found
            .iter()
            .any(|seen| seen.kind == cause.kind && seen.detail == cause.detail)
        {
            found.push(cause);
        }
    }
    found
}

/// Whether some claim gives `reason` for not being verified.
fn said(claims: &[ClaimRecord], reason: &str) -> bool {
    claims
        .iter()
        .any(|claim| claim.reason.as_deref() == Some(reason))
}

/// Why one claim was not verified. The proofs that run after the step's own
/// proof passed say more than its exit, so they are read first.
fn claim_cause(claim: &ClaimRecord, log: &dyn Fn(&str) -> String, run: &str) -> Cause {
    if let Some(differed) = &claim.differed {
        return Cause::new(
            Kind::ProofChanged,
            format!(
                "its proof differs from {}: {}",
                short(&differed.commit),
                differed.reasons.join("; ")
            ),
            format!("flot verify{run}; a person reviews the change, which no run proves"),
        );
    }
    if claim.verify_command.is_none() {
        return Cause::new(
            Kind::NoProof,
            format!("no verify command proves \"{}\"", claim.text),
            "declare `verify` for the step in the flow",
        );
    }
    if let Some(held) = claim
        .held
        .as_ref()
        .filter(|held| held.exit_code.is_some_and(|code| code != 0))
    {
        return Cause::new(
            Kind::Held,
            format!(
                "the proof fails with {} as the revision has them: the work passes only the versions it changed",
                held.files.join(", ")
            ),
            log(" --held"),
        );
    }
    if let Some(before) = claim
        .before
        .as_ref()
        .filter(|before| before.exit_code == Some(0))
    {
        return Cause::new(
            Kind::FailsBefore,
            format!(
                "the proof passes with the changes since {} undone, so it shows nothing of them",
                short(&before.commit)
            ),
            format!("flot cat {}", before.output_digest),
        );
    }
    if let Some(mutated) = &claim.mutants {
        let detail = if !mutated.survived.is_empty() {
            let mut named: Vec<String> = mutated
                .survived
                .iter()
                .take(NAMED_SURVIVORS)
                .map(|survivor| {
                    format!("{}:{}: {}", survivor.file, survivor.line, survivor.mutation)
                })
                .collect();
            let more = mutated.survived.len().saturating_sub(NAMED_SURVIVORS);
            if more > 0 {
                named.push(format!("and {more} more"));
            }
            format!(
                "{} the proof did not catch: {}",
                counted(mutated.survived.len(), "mutant"),
                named.join("; ")
            )
        } else if mutated.exit_code.is_none() {
            "the mutation run did not finish".into()
        } else {
            claim
                .reason
                .clone()
                .unwrap_or_else(|| "the mutation run did not hold".into())
        };
        return Cause::new(Kind::Mutants, detail, log(" --mutants"));
    }
    if claim.status == ClaimStatus::Refuted {
        let detail = match claim.verify_exit {
            Some(code) => format!("its proof exited {code}"),
            None => "its proof rejected it".into(),
        };
        return Cause::new(Kind::Refuted, detail, log(" --proof"));
    }
    Cause::new(
        Kind::Unproven,
        claim
            .reason
            .clone()
            .unwrap_or_else(|| "nothing proved it".into()),
        log(" --proof"),
    )
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

fn counted(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}
