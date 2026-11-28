//! Tests of the MCP server, with the helpers they share.

use serde_json::{json, Value};

use crate::service::Workspace;

use super::Server;

mod anchoring;
mod calls;
mod catalog;
mod protocol;
mod reading;
mod runs;

fn project(flow: &str) -> (tempfile::TempDir, Server) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("flot.toml"), flow).unwrap();
    let server = Server::new(Workspace::at(dir.path(), &dir.path().join("flot.toml")));
    (dir, server)
}

fn request(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
}

fn call(server: &Server, name: &str, arguments: Value) -> Value {
    let response = server
        .handle(&request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        ))
        .expect("a request gets a response");
    response["result"].clone()
}

const PROVEN: &str = r#"
[flow]
name = "mcp-demo"

[[step]]
id = "write"
run = "echo flot | tee artifact.txt"
claims = ["artifact.txt mentions flot"]
verify = "grep -q flot artifact.txt"
"#;
