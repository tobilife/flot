//! The guard: what an agent may touch, and the hook that asks it.

#[path = "guard/commands.rs"]
mod commands;
#[path = "guard/evidence.rs"]
mod evidence;
#[path = "guard/hook.rs"]
mod hook;
#[path = "guard/paths.rs"]
mod paths;
#[path = "guard/protected.rs"]
mod protected;
#[path = "guard/record.rs"]
mod record;

use crate::support::{flot_bin, git_in, Output, Project};
use std::path::Path;
use std::process::Command;

/// A flow whose proof reads `check.sh` and `tests/accept.sh`, in a
/// repository with a CI workflow and Claude Code settings.
const TWO_PROOFS: &str = r#"
[flow]
name = "g"

[[step]]
id = "work"
run = "echo 1 > out.txt"
claims = ["out.txt holds 1"]
verify = "sh check.sh && sh tests/accept.sh"
verify_inputs = ["check.sh", "tests/accept.sh"]
"#;

fn two_proofs_project() -> Project {
    let project = Project::new(TWO_PROOFS);
    project.write("check.sh", "grep -q 1 out.txt\n");
    project.write("tests/accept.sh", "grep -q 1 out.txt\n");
    project.write(".github/workflows/ci.yml", "on: push\n");
    project.write(".claude/settings.json", "{}\n");
    git_in(project.path(), &["init", "-q"]);
    project
}

/// `flot` run in `dir` with `home` as the home directory, given `stdin`.
fn flot_from(project: &Project, dir: &Path, home: &Path, args: &[&str], stdin: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new(flot_bin())
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("FLOT_SIGNING_KEY", project.key_path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("flot runs");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("flot runs");
    Output {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// A Claude Code `PreToolUse` payload: `tool` called with `input` in `cwd`.
fn hook_payload(cwd: &Path, tool: &str, input: serde_json::Value) -> String {
    serde_json::json!({ "cwd": cwd, "tool_name": tool, "tool_input": input }).to_string()
}
