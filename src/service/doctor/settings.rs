//! The check of an agent's settings: whether a Claude Code settings file
//! leaves the guard hook running and holds no credentials.

use super::{check, Check, Status};
use crate::core::secrets;

/// What a Claude Code settings file does to the guard and to secrets.
pub(super) fn agent_settings(name: &str, bytes: &[u8]) -> Vec<Check> {
    let mut found = Vec::new();
    let settings: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(settings) => settings,
        Err(error) => {
            return vec![check(
                "agent settings",
                Status::Warn,
                format!("{name} is not JSON ({error}); Claude Code may ignore it"),
            )]
        }
    };
    if settings["disableAllHooks"] == true {
        found.push(check(
            "agent settings",
            Status::Fail,
            format!("{name} sets disableAllHooks, so the flot guard never runs"),
        ));
    }
    if settings["permissions"]["defaultMode"] == "bypassPermissions" {
        found.push(check(
            "agent settings",
            Status::Warn,
            format!("{name} runs every tool without asking (bypassPermissions); the guard hook is then all that stops an edit to a proof"),
        ));
    }
    let kinds: Vec<&str> = secrets::scan(bytes)
        .iter()
        .map(|finding| finding.kind)
        .collect();
    if !kinds.is_empty() {
        found.push(check(
            "agent settings",
            Status::Warn,
            format!(
                "{name} holds what looks like a credential ({})",
                kinds.join(", ")
            ),
        ));
    }
    if found.is_empty() {
        found.push(check(
            "agent settings",
            Status::Ok,
            format!("{name} leaves hooks on"),
        ));
    }
    found
}
