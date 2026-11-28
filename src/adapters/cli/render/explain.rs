//! A run's explanation as `flot explain` prints it: what keeps the run
//! from passing, then each step that did not hold, each cause with the
//! command to read next.

use crate::service::explain::{Cause, Explanation, Kind};

use super::super::terminal_safe;

/// What a cause is called on a terminal: its JSON name, in words.
fn named(kind: Kind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(|name| name.replace('_', " ")))
        .unwrap_or_default()
}

fn render_cause(cause: &Cause) {
    say!("  - {}: {}", named(cause.kind), cause.detail);
    say!("    next: {}", cause.next);
}

pub(in super::super) fn render_explanation(explanation: &Explanation) {
    say!(
        "{} {}",
        explanation.result.label().to_uppercase(),
        explanation.run_id
    );
    explanation.causes.iter().for_each(render_cause);
    for step in &explanation.steps {
        say!(
            "{:<20} {:<9} {}",
            step.step_id,
            step.status.label(),
            if step.cached { "cached" } else { "ran" }
        );
        if step.causes.is_empty() {
            say!("  held: every claim it makes was proven");
        }
        step.causes.iter().for_each(render_cause);
    }
    if explanation.causes.is_empty() && explanation.steps.is_empty() {
        say!("nothing to explain: every step held");
    }
}
