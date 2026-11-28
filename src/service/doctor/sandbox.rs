//! Whether this host can confine a step, checked before any step asks:
//! going without network (on Linux a user and network namespace) and
//! writing only where it may (Landlock ABI 3 on Linux, `sandbox-exec` on
//! macOS), each tried on a trivial child; and what the flow's steps that
//! ask for a sandbox need of them.

use std::path::Path;

use crate::core::flow::Flow;
use crate::engine::sandbox;

use super::{check, Check, Status};

/// What confining a step each way comes to here: the mechanism that
/// enforces it, or why it cannot be enforced.
pub(super) struct Support {
    pub(super) network: Result<String, String>,
    pub(super) write: Result<String, String>,
}

impl Support {
    pub(super) fn here(root: &Path) -> Self {
        let tried = |network, write| {
            sandbox::probe(root, network, write).map_err(|error| format!("{error:#}"))
        };
        Support {
            network: tried(false, false),
            write: tried(true, true),
        }
    }

    /// What a step can be confined to here, in words.
    fn described(&self) -> String {
        if let (Err(network), Err(write)) = (&self.network, &self.write) {
            if network == write {
                return format!("steps cannot be confined here: {network}");
            }
        }
        let part = |what: &str, found: &Result<String, String>| match found {
            Ok(mechanism) => format!("a step can {what} ({mechanism})"),
            Err(why) => format!("a step cannot {what}: {why}"),
        };
        format!(
            "{}; {}",
            part("go without network", &self.network),
            part("write only where it may", &self.write)
        )
    }
}

/// The sandbox check. Steps that ask for what this host cannot enforce
/// fail it, as they will not run; otherwise it says what the host can
/// confine a step to, a note when that is not everything.
pub(super) fn sandbox_check(flow: Option<&Flow>, support: &Support) -> Check {
    let steps = flow.map(|flow| flow.steps.as_slice()).unwrap_or_default();
    let asking = |needs: fn(&crate::core::flow::Sandbox) -> bool| -> Vec<&str> {
        steps
            .iter()
            .filter(|step| step.sandbox.as_ref().is_some_and(needs))
            .map(|step| step.id.as_str())
            .collect()
    };
    let offline = asking(|policy| !policy.network);
    let writing = asking(|policy| policy.write.is_some());
    let mut lacking = Vec::new();
    if let (false, Err(why)) = (offline.is_empty(), &support.network) {
        lacking.push(format!(
            "{} cannot go without network here: {why}",
            offline.join(", ")
        ));
    }
    if let (false, Err(why)) = (writing.is_empty(), &support.write) {
        lacking.push(format!(
            "{} cannot be held to their writes here: {why}",
            writing.join(", ")
        ));
    }
    if !lacking.is_empty() {
        return check(
            "sandbox",
            Status::Fail,
            format!(
                "{}; a step that asked to be confined will not run",
                lacking.join("; ")
            ),
        );
    }
    let asked = asking(|_| true);
    if asked.is_empty() {
        let status = match (&support.network, &support.write) {
            (Ok(_), Ok(_)) => Status::Ok,
            _ => Status::Note,
        };
        return check(
            "sandbox",
            status,
            format!("no step asks for one; {}", support.described()),
        );
    }
    check(
        "sandbox",
        Status::Ok,
        format!("{}: enforced; {}", asked.join(", "), support.described()),
    )
}

#[cfg(test)]
mod tests;
