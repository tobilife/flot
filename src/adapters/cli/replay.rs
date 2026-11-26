//! `flot guard --replay`: the tool calls in Claude Code transcripts,
//! judged as the hook would judge them now.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::service::Workspace;

use super::hook::{cannot_judge, judge, unpanicked, Call, GUARDED_TOOLS};
use super::render::print_json;
use super::terminal_safe;

/// One tool call the hook would have refused, read back from a transcript.
#[derive(Serialize)]
struct Replayed {
    transcript: String,
    at: String,
    tool: String,
    cwd: String,
    reasons: Vec<String>,
}

#[derive(Serialize, Default)]
struct Replay {
    transcripts: usize,
    /// Calls to the tools the hook sees.
    calls: usize,
    /// Of those, calls where a flow was found.
    in_a_flow: usize,
    /// Calls whose working directory is gone, so they were not judged.
    unjudged: usize,
    refused: Vec<Replayed>,
}

/// Judge every edit, shell command and other call naming a path in Claude
/// Code transcripts (JSONL files, or directories holding them; by default
/// `~/.claude/projects`) as the hook would today, against the flows as they
/// are now.
pub(super) fn replay_transcripts(paths: &[PathBuf], json: bool) -> Result<ExitCode> {
    let roots: Vec<PathBuf> = if paths.is_empty() {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .context("no home directory to find ~/.claude/projects in; name the transcripts")?;
        vec![PathBuf::from(home).join(".claude").join("projects")]
    } else {
        paths.to_vec()
    };
    let mut files = Vec::new();
    for root in &roots {
        transcripts_in(root, &mut files)
            .with_context(|| format!("cannot read {}", root.display()))?;
    }
    files.sort();
    let mut replay = Replay {
        transcripts: files.len(),
        ..Replay::default()
    };
    let mut seen = std::collections::HashSet::new();
    for file in &files {
        let text =
            std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
        for line in text.split(|&byte| byte == b'\n') {
            let Ok(entry) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            if entry["type"] != "assistant" {
                continue;
            }
            let Some(content) = entry["message"]["content"].as_array() else {
                continue;
            };
            for item in content {
                let tool = item["name"].as_str().unwrap_or_default();
                if item["type"] != "tool_use" {
                    continue;
                }
                // Another tool counts when it names a path it may change.
                // One that only reads counts when what it reads is a
                // secret, which is all it is judged for.
                let cwd = entry["cwd"].as_str().unwrap_or_default();
                let base = PathBuf::from(cwd);
                let call = Call::of(&base, tool, &item["input"]);
                let reads = call
                    .as_ref()
                    .is_ok_and(|call| call.is_empty() && !call.named.is_empty());
                if !GUARDED_TOOLS.contains(&tool)
                    && call.as_ref().map_or(true, Call::is_empty)
                    && !reads
                {
                    continue;
                }
                // A call is written again when its message is.
                if let Some(id) = item["id"].as_str() {
                    if !seen.insert(id.to_string()) {
                        continue;
                    }
                }
                if cwd.is_empty() || !base.is_dir() {
                    if !reads {
                        replay.calls += 1;
                        replay.unjudged += 1;
                    }
                    continue;
                }
                let workspace = Workspace::locate(&base, None)?;
                // As the hook does, a call that cannot be judged is refused
                // where a flow is.
                let (in_a_flow, reasons) = match unpanicked(|| judge(&workspace, true, call?)) {
                    Ok(judged) => (!judged.flows.is_empty(), judged.reasons()),
                    Err(error) => {
                        let mut guard = crate::service::Guard::new(&workspace);
                        guard.consult(&base);
                        let reasons = if guard.active() {
                            vec![cannot_judge(&error)]
                        } else {
                            Vec::new()
                        };
                        (guard.active(), reasons)
                    }
                };
                if reads && reasons.is_empty() {
                    continue;
                }
                replay.calls += 1;
                if in_a_flow {
                    replay.in_a_flow += 1;
                }
                if !reasons.is_empty() {
                    replay.refused.push(Replayed {
                        transcript: file.display().to_string(),
                        at: entry["timestamp"].as_str().unwrap_or_default().to_string(),
                        tool: tool.to_string(),
                        cwd: cwd.to_string(),
                        reasons,
                    });
                }
            }
        }
    }
    if json {
        print_json(&replay)?;
    } else {
        for found in &replay.refused {
            say!(
                "refused  {}  {}  {}: {}",
                found.at,
                found.tool,
                found.cwd,
                found.reasons.join("; ")
            );
        }
        say!(
            "{} transcripts, {} edits and commands: {} where a flow is, {} refused, {} not judged because their directory is gone",
            replay.transcripts,
            replay.calls,
            replay.in_a_flow,
            replay.refused.len(),
            replay.unjudged
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// The `.jsonl` files at `path`: the file itself, or those beneath it.
fn transcripts_in(path: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !path.is_dir() {
        std::fs::metadata(path)?;
        files.push(path.to_path_buf());
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            transcripts_in(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            files.push(path);
        }
    }
    Ok(())
}
