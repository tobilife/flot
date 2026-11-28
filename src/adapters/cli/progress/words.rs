//! The words both views tell a step's progress in: how it came out, which
//! attempt or candidate it is, its claims, its time, and the tag of its lines.

use crate::core::receipt::{ClaimCounts, StepStatus};
use crate::service::progress::Source;

/// What a step came to, in a word: one that is `ok` passed only once none of
/// its claims is left unverified or refuted.
pub(super) fn outcome(status: StepStatus, claims: ClaimCounts) -> &'static str {
    match status {
        StepStatus::Ok if claims.refuted > 0 => "refuted",
        StepStatus::Ok if claims.unverified > 0 => "unverified",
        StepStatus::Ok => "passed",
        other => other.label(),
    }
}

/// ` attempt 2/3` for a step that may take more than one, and
/// ` candidate NAME` for one of a step's candidates.
pub(super) fn which(attempt: u32, attempts: u32, candidate: Option<&str>) -> String {
    let mut text = String::new();
    if attempt > 0 && attempts > 1 {
        text.push_str(&format!(" attempt {attempt}/{attempts}"));
    }
    if let Some(name) = candidate {
        text.push_str(&format!(" candidate {name}"));
    }
    text
}

/// ` claims 2/3`: how many of its claims held, of how many, when it makes any.
pub(super) fn held(claims: ClaimCounts) -> String {
    match claims.total() {
        0 => String::new(),
        total => format!(" claims {}/{total}", claims.verified),
    }
}

/// How a step ended: its outcome, time and claims, or, for one that never
/// executed, why it was not run, which says more than `cancelled`.
pub(super) fn ended(
    status: StepStatus,
    attempt: u32,
    duration_ms: u64,
    claims: ClaimCounts,
    why: Option<&str>,
) -> String {
    if let Some(why) = why {
        return why.to_string();
    }
    // A step that never executed took no time worth telling.
    let took = match attempt {
        0 => String::new(),
        _ => format!(" {}", seconds(duration_ms)),
    };
    format!("{}{took}{}", outcome(status, claims), held(claims))
}

/// `52ms` under a tenth of a second, `1.2s` from there.
pub(super) fn seconds(millis: u64) -> String {
    match millis {
        0..100 => format!("{millis}ms"),
        _ => format!("{}.{}s", millis / 1000, millis % 1000 / 100),
    }
}

/// `[build]`, `[build:stderr]` or `[build:proof]`.
pub(super) fn tag(step: &str, source: Source) -> String {
    match source {
        Source::Stdout => format!("[{step}]"),
        Source::Stderr => format!("[{step}:stderr]"),
        Source::Proof => format!("[{step}:proof]"),
    }
}
