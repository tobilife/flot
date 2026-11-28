//! The hook that asks the guard about an agent's tool call.

use super::{flot_from, hook_payload, two_proofs_project};
use std::path::Path;

#[test]
fn the_hook_refuses_a_call_it_cannot_judge_where_a_flow_is() {
    let project = two_proofs_project();
    let hook = |payload: &str| project.flot_stdin(&["guard", "--hook"], payload);
    let bash = |command: &str| {
        hook_payload(
            project.path(),
            "Bash",
            serde_json::json!({ "command": command }),
        )
    };
    // A command cut in the middle of an emoji carries a lone surrogate,
    // which JSON parsers refuse. It is repaired, and the command judged.
    for half in ["\\ud83d", "\\ude00", "\\ud83d\\ud83d"] {
        hook(&bash("rm check.sh # X").replace(" # X", &format!(" # {half}")))
            .ok()
            .has("\"permissionDecision\": \"deny\"")
            .has("check.sh is protected: the proof of step `work` reads it");
    }
    // A whole pair stands, and an escaped backslash before `u` is text.
    for allowed in ["echo \u{1F600} > notes.md", "echo \\ud83d > notes.md"] {
        let answer = hook(&bash(allowed));
        assert_eq!(answer.ok().stdout.trim(), "", "{allowed}");
    }
    for broken in [
        "not json".to_string(),
        serde_json::json!({ "cwd": project.path(), "tool_input": { "command": "true" } })
            .to_string(),
        serde_json::json!({ "cwd": project.path(), "tool_name": "Bash" }).to_string(),
        hook_payload(
            project.path(),
            "Edit",
            serde_json::json!({ "old_string": "a" }),
        ),
        hook_payload(project.path(), "Bash", serde_json::json!({ "command": 7 })),
    ] {
        hook(&broken)
            .ok()
            .has("\"permissionDecision\": \"deny\"")
            .has("this call cannot be judged");
    }
    // Outside any flow, the hook says nothing, whatever it is given.
    let elsewhere = tempfile::tempdir().unwrap();
    let outside = flot_from(
        &project,
        elsewhere.path(),
        elsewhere.path(),
        &["guard", "--hook"],
        "not json",
    );
    assert_eq!(
        (outside.code, outside.stdout.trim()),
        (0, ""),
        "{}",
        outside.stderr
    );
}

#[test]
fn the_hook_judges_every_tool_that_names_a_path() {
    let project = two_proofs_project();
    let hook = |tool: &str, input: serde_json::Value| {
        project.flot_stdin(
            &["guard", "--hook"],
            &hook_payload(project.path(), tool, input),
        )
    };
    let check = project.path().join("check.sh");
    for (tool, input, said) in [
        (
            "mcp__filesystem__write_file",
            serde_json::json!({ "path": check, "content": "exit 0" }),
            "check.sh is protected",
        ),
        (
            "mcp__filesystem__edit_file",
            serde_json::json!({ "path": "check.sh", "edits": [] }),
            "check.sh is protected",
        ),
        (
            "mcp__filesystem__move_file",
            serde_json::json!({ "source": "x.sh", "destination": "tests/accept.sh" }),
            "tests/accept.sh is protected",
        ),
        (
            "mcp__fs__delete",
            serde_json::json!({ "paths": ["notes.md", "tests"] }),
            "tests is protected: it holds tests/accept.sh",
        ),
        (
            "mcp__notes__save",
            serde_json::json!({ "options": { "file": "check.sh" } }),
            "check.sh is protected",
        ),
        (
            "mcp__x__updateWidget",
            serde_json::json!({ "target": ".claude/settings.json" }),
            ".claude/settings.json is protected",
        ),
    ] {
        hook(tool, input)
            .ok()
            .has("\"permissionDecision\": \"deny\"")
            .has(said);
    }
    for (tool, input) in [
        (
            "mcp__filesystem__read_file",
            serde_json::json!({ "path": check }),
        ),
        (
            "mcp__filesystem__list_directory",
            serde_json::json!({ "path": "." }),
        ),
        (
            "mcp__filesystem__directory_tree",
            serde_json::json!({ "path": "." }),
        ),
        (
            "mcp__filesystem__get_file_info",
            serde_json::json!({ "path": "check.sh" }),
        ),
        (
            "mcp__filesystem__search_files",
            serde_json::json!({ "path": ".", "pattern": "x" }),
        ),
        (
            "mcp__filesystem__write_file",
            serde_json::json!({ "path": "notes.md", "content": "x" }),
        ),
        ("Read", serde_json::json!({ "file_path": check })),
        ("LS", serde_json::json!({ "path": project.path() })),
        (
            "Glob",
            serde_json::json!({ "pattern": "*.sh", "path": project.path() }),
        ),
        (
            "Grep",
            serde_json::json!({ "pattern": "x", "path": project.path() }),
        ),
        (
            "WebFetch",
            serde_json::json!({ "url": "https://x.example", "prompt": "x" }),
        ),
        ("TodoWrite", serde_json::json!({ "todos": [] })),
    ] {
        let answer = hook(tool, input);
        assert_eq!(answer.ok().stdout.trim(), "", "{tool}");
    }

    // The plugin's hook is asked about every tool.
    let hooks: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/claude-code/hooks/hooks.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(hooks["hooks"]["PreToolUse"][0]["matcher"], ".*");

    // And a transcript is judged as the hook would.
    let cwd = project.path().to_str().unwrap();
    let call = |id: &str, name: &str, input: serde_json::Value| {
        serde_json::json!({
            "type": "assistant",
            "cwd": cwd,
            "timestamp": "2026-11-20T10:00:00.000Z",
            "message": { "content": [
                { "type": "tool_use", "id": id, "name": name, "input": input },
            ] },
        })
        .to_string()
    };
    let transcript = [
        call(
            "m1",
            "mcp__filesystem__write_file",
            serde_json::json!({ "path": "check.sh", "content": "exit 0" }),
        ),
        call(
            "m2",
            "mcp__filesystem__read_file",
            serde_json::json!({ "path": "check.sh" }),
        ),
        call("m3", "TodoWrite", serde_json::json!({ "todos": [] })),
    ]
    .join("\n");
    let transcripts = tempfile::tempdir().unwrap();
    std::fs::write(transcripts.path().join("s.jsonl"), transcript).unwrap();
    let replay = project
        .flot(&[
            "guard",
            "--replay",
            "--json",
            transcripts.path().to_str().unwrap(),
        ])
        .ok()
        .json();
    assert_eq!(replay["calls"], 1, "{replay}");
    assert_eq!(replay["refused"][0]["tool"], "mcp__filesystem__write_file");
}
