//! A Claude Code tool's input read into a call: an edit by its file, a
//! shell by its command line, and any other tool by the paths it names.

use std::path::Path;

use anyhow::{Context, Result};

use crate::service::{tilde, Call, Shell};

/// The tools the Claude Code hook reads by their kind: an edit names its
/// file, a shell its command line. Any other tool is read for the paths its
/// input names (see [`PATH_KEYS`]), unless it is named like a read.
pub(in super::super) const GUARDED_TOOLS: [&str; 6] = [
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
pub(super) fn reads_only(tool: &str) -> bool {
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
pub(super) fn words_of(name: &str) -> Vec<String> {
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
pub(in super::super) fn tool_call(
    base: &Path,
    tool: &str,
    input: &serde_json::Value,
) -> Result<Call> {
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
