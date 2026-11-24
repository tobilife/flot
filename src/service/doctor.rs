//! What in this setup weakens the evidence, checked without running a step.
//!
//! Each check names something flot relies on and says whether it holds here:
//! the flow parses, proofs are held to a revision, the store stays out of
//! commits, a requested sandbox can be enforced, and the agent's settings
//! leave the guard hook running. Nothing is changed.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::secrets;
use crate::engine::git::Git;
use crate::engine::sandbox;
use crate::service::{keys, Workspace};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    /// Worth knowing; nothing is wrong.
    Note,
    /// The evidence is weaker than it could be.
    Warn,
    /// Something flot relies on does not hold.
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub check: &'static str,
    pub status: Status,
    pub detail: String,
}

fn counted(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

fn check(check: &'static str, status: Status, detail: impl Into<String>) -> Check {
    Check {
        check,
        status,
        detail: detail.into(),
    }
}

impl Workspace {
    pub fn doctor(&self) -> Vec<Check> {
        let mut checks = Vec::new();
        let flow = match self.flow() {
            Ok(flow) => {
                let claims: usize = flow.steps.iter().map(|step| step.claims.len()).sum();
                let steps = flow.steps.len();
                checks.push(check(
                    "flow",
                    Status::Ok,
                    format!(
                        "{}: {}, {}",
                        self.flow_path.display(),
                        counted(steps, "step"),
                        counted(claims, "claim")
                    ),
                ));
                Some(flow)
            }
            Err(error) => {
                checks.push(check("flow", Status::Fail, format!("{error:#}")));
                None
            }
        };

        if let Some(flow) = &flow {
            checks.push(match self.baseline(Some(flow), None) {
                Ok(Some(baseline)) => check(
                    "proofs",
                    Status::Ok,
                    format!("held to {}", baseline.label()),
                ),
                Ok(None) => check(
                    "proofs",
                    Status::Warn,
                    "proofs_from is not set: a proof can be changed along with the work it proves",
                ),
                Err(error) => check("proofs", Status::Fail, format!("{error:#}")),
            });
        }

        if let Ok(git) = Git::open(&self.root) {
            let store = crate::store::STORE_DIR;
            if git.run(&["check-ignore", "-q", store], None).is_err() {
                checks.push(check(
                    "store",
                    Status::Warn,
                    format!("git does not ignore {store}/, so `git add -A` would commit the store"),
                ));
            } else {
                checks.push(check(
                    "store",
                    Status::Ok,
                    format!("{store}/ is ignored by git"),
                ));
            }
        }

        checks.push(match keys::default_path() {
            Ok(path) if !path.exists() => check(
                "signing key",
                Status::Note,
                "none: runs cannot be signed (`flot key init`)",
            ),
            Ok(path) => match keys::load(&path) {
                Ok(key) => check(
                    "signing key",
                    Status::Ok,
                    format!("{} at {}", key.public_key(), path.display()),
                ),
                Err(error) => check("signing key", Status::Fail, format!("{error:#}")),
            },
            Err(error) => check("signing key", Status::Fail, format!("{error:#}")),
        });

        // Opening a store creates one; the doctor only reads.
        let stored = self.root.join(crate::store::STORE_DIR).is_dir();
        checks.push(if !stored {
            check("trust", Status::Note, "no store yet")
        } else {
            match self.trust_list() {
                Ok(list) if list.is_empty() => check(
                    "trust",
                    Status::Note,
                    "no signers trusted: imported runs are evidence, never cache",
                ),
                Ok(list) => {
                    let names: Vec<&str> = list.iter().map(|entry| entry.name.as_str()).collect();
                    check(
                        "trust",
                        Status::Ok,
                        format!("trusting {}", names.join(", ")),
                    )
                }
                Err(error) => check("trust", Status::Fail, format!("{error:#}")),
            }
        });

        if let Some(flow) = &flow {
            let confined: Vec<&str> = flow
                .steps
                .iter()
                .filter(|step| step.sandbox.is_some())
                .map(|step| step.id.as_str())
                .collect();
            if !confined.is_empty() {
                let steps = confined.join(", ");
                checks.push(match sandbox::probe(&self.root) {
                    Ok(mechanism) => check(
                        "sandbox",
                        Status::Ok,
                        format!("{steps}: enforced by {mechanism}"),
                    ),
                    Err(error) => check(
                        "sandbox",
                        Status::Fail,
                        format!(
                            "{steps}: cannot be enforced here, so they will not run: {error:#}"
                        ),
                    ),
                });
            }
        }

        let top = self
            .root
            .ancestors()
            .find(|dir| dir.join(".git").is_dir())
            .unwrap_or(&self.root);
        let mut settings: Vec<(String, PathBuf)> =
            [".claude/settings.json", ".claude/settings.local.json"]
                .into_iter()
                .map(|name| (name.to_string(), top.join(name)))
                .collect();
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            settings.push((
                "~/.claude/settings.json".to_string(),
                Path::new(&home).join(".claude/settings.json"),
            ));
        }
        for (name, path) in settings {
            if let Ok(bytes) = std::fs::read(&path) {
                checks.extend(agent_settings(&name, &bytes));
            }
        }
        checks
    }
}

/// What a Claude Code settings file does to the guard and to secrets.
fn agent_settings(name: &str, bytes: &[u8]) -> Vec<Check> {
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
