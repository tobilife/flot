//! The flow served over MCP.

use crate::support::{flot_bin, Project, PROVEN};
use std::process::Command;

#[test]
fn the_mcp_server_handshakes_and_runs_the_flow_over_stdio() {
    use std::io::Write;

    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");

    let mut child = Command::new(flot_bin())
        .arg("mcp")
        .current_dir(project.path())
        .env("FLOT_SIGNING_KEY", project.key_path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("flot mcp starts");

    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"flot_run","arguments":{}}}"#,
    ];
    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        for request in requests {
            writeln!(stdin, "{request}").unwrap();
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("mcp server exits");
    assert!(output.status.success());

    let replies: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is JSON-RPC"))
        .collect();
    assert_eq!(replies.len(), 3, "the notification must not be answered");
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "flot");
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 11);
    assert_eq!(
        replies[2]["result"]["structuredContent"]["manifest"]["verdict"],
        "passed"
    );
    assert!(project.path().join(".flot/runs").exists());
}
