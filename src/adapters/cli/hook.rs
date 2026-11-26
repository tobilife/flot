//! The guard's Claude Code front end: a hook payload, or a tool's input in
//! a transcript, read into a call for the service to judge, and the hook's
//! reply.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};

use crate::service::{answer, judge, tilde, Call, Shell, Workspace};

use super::render::print_json;

/// The tools the Claude Code hook reads by their kind: an edit names its
/// file, a shell its command line. Any other tool is read for the paths its
/// input names (see [`PATH_KEYS`]), unless it is named like a read.
pub(super) const GUARDED_TOOLS: [&str; 6] = [
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "Bash",
    "PowerShell",
];

/// The keys whose string values, at any depth of another tool's input, are
/// paths it may change. A `source` is read by a copy but removed by a move.
const PATH_KEYS: [&str; 9] = [
    "path",
    "file_path",
    "file",
    "filename",
    "destination",
    "target",
    "notebook_path",
    "paths",
    "source",
];

/// Whether `tool` is named like one that only reads, such as `Read`, `LS`,
/// `WebFetch` or `mcp__filesystem__list_directory`: a word of its name
/// starts with read, get, list, search, view, find, grep, glob or fetch, or
/// is ls, tree, stat or info, and none names a change, as in
/// `search_replace` or `read_and_write`.
fn reads_only(tool: &str) -> bool {
    const READS: [&str; 9] = [
        "read", "get", "list", "search", "view", "find", "grep", "glob", "fetch",
    ];
    const CHANGES: [&str; 33] = [
        "write", "edit", "create", "delete", "remove", "move", "rename", "replace", "update",
        "set", "put", "patch", "apply", "append", "insert", "save", "upload", "download", "copy",
        "mkdir", "rm", "mv", "cp", "touch", "exec", "run", "modify", "change", "add", "restore",
        "reset", "link", "unlink",
    ];
    let words = words_of(tool);
    words.iter().any(|word| {
        matches!(word.as_str(), "ls" | "tree" | "stat" | "info")
            || READS.iter().any(|read| word.starts_with(read))
    }) && !words
        .iter()
        .any(|word| CHANGES.iter().any(|change| word.starts_with(change)))
}

/// `name` in lowercase words, split at what is not a letter or digit and
/// where camelCase turns upper.
fn words_of(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut after_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            after_lower = false;
            continue;
        }
        if c.is_uppercase() && after_lower {
            words.push(std::mem::take(&mut word));
        }
        after_lower = c.is_lowercase() || c.is_ascii_digit();
        word.extend(c.to_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Every string under a key in [`PATH_KEYS`] in a tool's `input`, at any
/// depth.
fn named_paths(value: &serde_json::Value, depth: u8, found: &mut dyn FnMut(&str)) {
    fn strings(value: &serde_json::Value, found: &mut dyn FnMut(&str)) {
        match value {
            serde_json::Value::String(text) if !text.is_empty() => found(text),
            serde_json::Value::Array(items) => {
                for item in items {
                    strings(item, found);
                }
            }
            _ => {}
        }
    }
    if depth > 32 {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if PATH_KEYS.contains(&key.as_str()) {
                    strings(value, found);
                }
                named_paths(value, depth + 1, found);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                named_paths(item, depth + 1, found);
            }
        }
        _ => {}
    }
}

/// A `tool`'s input as Claude Code gives it, run from `base`. An edit or
/// a shell call without the file or command line its tool always names
/// cannot be judged, which is an error.
pub(super) fn tool_call(base: &Path, tool: &str, input: &serde_json::Value) -> Result<Call> {
    let mut call = Call::default();
    let named = |key: &str| {
        input[key]
            .as_str()
            .with_context(|| format!("the {tool} call names no {key}"))
    };
    match tool {
        "Edit" | "Write" | "MultiEdit" => call.targets.push(base.join(tilde(named("file_path")?))),
        "NotebookEdit" => call.targets.push(base.join(tilde(named("notebook_path")?))),
        "Bash" | "PowerShell" => {
            let shell = if tool == "PowerShell" {
                Shell::PowerShell
            } else {
                Shell::Posix
            };
            call.commands
                .push((base.to_path_buf(), named("command")?.to_string(), shell));
        }
        _ if reads_only(tool) => named_paths(input, 0, &mut |path| {
            call.named.push(base.join(tilde(path)));
        }),
        _ => named_paths(input, 0, &mut |path| {
            call.targets.push(base.join(tilde(path)));
        }),
    }
    Ok(call)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_surrogate_escape_is_replaced_and_a_pair_is_kept() {
        assert_eq!(
            repair_surrogates(r#"{"a":"x\ud83d"}"#),
            r#"{"a":"x\ufffd"}"#
        );
        assert_eq!(
            repair_surrogates(r#""\ude00 \ud83d\ude00""#),
            r#""\ufffd \ud83d\ude00""#
        );
        assert_eq!(repair_surrogates(r#""\\ud83d""#), r#""\\ud83d""#);
        assert_eq!(repair_surrogates(r#""\ud83d"#), r#""\ufffd"#);
        let value: serde_json::Value =
            serde_json::from_str(&repair_surrogates(r#"{"c":"rm check.sh \ud83d"}"#)).unwrap();
        assert_eq!(value["c"], "rm check.sh \u{fffd}");
    }

    #[test]
    fn a_panic_in_the_guard_is_an_error() {
        let failed = unpanicked(|| -> anyhow::Result<()> { panic!("boom") });
        assert!(format!("{:#}", failed.unwrap_err()).contains("boom"));
        assert_eq!(unpanicked(|| Ok(1)).unwrap(), 1);
    }

    #[test]
    fn a_tool_is_read_only_by_its_name_unless_it_names_a_change() {
        assert_eq!(
            words_of("mcp__fs__read_text_file"),
            ["mcp", "fs", "read", "text", "file"]
        );
        assert_eq!(words_of("NotebookEdit"), ["notebook", "edit"]);
        for tool in [
            "Read",
            "LS",
            "Glob",
            "Grep",
            "WebFetch",
            "mcp__fs__list_directory",
        ] {
            assert!(reads_only(tool), "{tool}");
        }
        for tool in [
            "Write",
            "mcp__fs__write_file",
            "mcp__fs__move_file",
            "search_replace",
            "readAndWrite",
            "download_file",
            "TodoWrite",
        ] {
            assert!(!reads_only(tool), "{tool}");
        }
    }
}
