//! The guard's Claude Code front end: a hook payload, or a tool's input in
//! a transcript, read into a call for the service to judge, and the hook's
//! reply.

#[cfg(test)]
mod tests;
mod tool;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};

use crate::service::{answer, judge, Call, Workspace};

use super::render::print_json;

pub(super) use tool::{tool_call, GUARDED_TOOLS};

/// `work`'s result, with a panic in it as an error: the hook answers either
/// way.
pub(super) fn unpanicked<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|panic| {
        let said = panic
            .downcast_ref::<&str>()
            .map(|said| said.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic".to_string());
        Err(anyhow::anyhow!("the guard failed: {said}"))
    })
}

/// `payload` with each lone UTF-16 surrogate escape, such as the half of an
/// emoji a cut command ends in, replaced by `\ufffd`: a JSON parser refuses
/// one, and the rest of the payload must still be judged.
fn repair_surrogates(payload: &str) -> std::borrow::Cow<'_, str> {
    let bytes = payload.as_bytes();
    // The code unit of a `\uXXXX` escape at `at`.
    let unit = |at: usize| -> Option<u16> {
        if bytes.get(at) != Some(&b'\\') || bytes.get(at + 1) != Some(&b'u') {
            return None;
        }
        let hex = payload.get(at + 2..at + 6)?;
        if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        u16::from_str_radix(hex, 16).ok()
    };
    let mut repaired = String::new();
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'\\' {
            at += 1;
            continue;
        }
        match unit(at) {
            Some(0xD800..=0xDBFF) if matches!(unit(at + 6), Some(0xDC00..=0xDFFF)) => at += 12,
            Some(0xD800..=0xDFFF) => {
                repaired.push_str(&payload[copied..at]);
                repaired.push_str("\\ufffd");
                at += 6;
                copied = at;
            }
            // Any other escape, `\\` among them, is two characters at least.
            _ => at += 2,
        }
    }
    if copied == 0 {
        return std::borrow::Cow::Borrowed(payload);
    }
    repaired.push_str(&payload[copied..]);
    std::borrow::Cow::Owned(repaired)
}

/// Answer a Claude Code `PreToolUse` payload on stdin, for the tool call
/// it names and whatever `call` already holds: a denial on stdout, noted
/// in the store for the next run to record, or nothing. The exit stays 0,
/// so the hook's `|| true` keeps a failure to run flot at all from blocking
/// every call. A payload that does not parse is a call flot cannot judge,
/// which [`answer`] refuses where a flow is.
pub(super) fn answer_hook(
    workspace: &Workspace,
    nearest: bool,
    cwd: &Path,
    call: Call,
) -> Result<ExitCode> {
    let mut base = cwd.to_path_buf();
    let mut tool = String::from("a tool");
    let judged = unpanicked(|| {
        let mut payload = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut payload)
            .context("cannot read the hook payload")?;
        let payload: serde_json::Value = serde_json::from_str(&repair_surrogates(&payload))
            .context("the hook payload is not JSON")?;
        if let Some(dir) = payload["cwd"].as_str() {
            base = PathBuf::from(dir);
        }
        tool = payload["tool_name"]
            .as_str()
            .context("the hook payload names no tool_name")?
            .to_string();
        anyhow::ensure!(
            payload["tool_input"].is_object(),
            "the hook payload has no tool_input"
        );
        let asked = tool_call(&base, &tool, &payload["tool_input"])?;
        let mut call = call;
        call.targets.extend(asked.targets);
        call.commands.extend(asked.commands);
        call.named.extend(asked.named);
        judge(workspace, nearest, call)
    });
    let answer = answer(workspace, &[&base, cwd], judged);
    if answer.reasons.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    print_json(&serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": answer.denial,
        }
    }))?;
    answer.note(&tool);
    Ok(ExitCode::SUCCESS)
}
