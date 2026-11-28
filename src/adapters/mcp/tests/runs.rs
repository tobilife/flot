//! Running and checking a flow over MCP: the verdict and result the CLI
//! would give, the flow's cost ceiling, and tool errors.

use serde_json::json;

use super::{call, project, PROVEN};

#[test]
fn run_returns_the_same_verdict_the_cli_would() {
    let (_dir, server) = project(PROVEN);
    let result = call(&server, "flot_run", json!({}));
    assert_eq!(result["isError"], false);
    let manifest = &result["structuredContent"]["manifest"];
    assert_eq!(manifest["verdict"], "passed");
    assert_eq!(manifest["claims"]["verified"], 1);
    assert!(!result["content"][0]["text"].as_str().unwrap().is_empty());
}

#[test]
fn an_unproven_claim_is_reported_as_unverified_not_as_an_error() {
    let (_dir, server) = project(
        r#"
[flow]
name = "unproven"

[[step]]
id = "assert"
run = "true"
claims = ["the migration is safe"]
"#,
    );
    let result = call(&server, "flot_run", json!({}));
    assert_eq!(result["isError"], false);
    assert_eq!(
        result["structuredContent"]["manifest"]["verdict"],
        "unverified"
    );
}

#[test]
fn verify_authenticates_the_run_the_server_just_recorded() {
    let (_dir, server) = project(PROVEN);
    call(&server, "flot_run", json!({}));
    let audit = call(&server, "flot_verify", json!({}))["structuredContent"].clone();
    assert_eq!(audit["intact"], true);
    assert_eq!(audit["verdict"], "passed");
    assert_eq!(audit["result"], "passed");
}

#[test]
fn verify_of_tampered_evidence_is_broken_and_an_error() {
    let (dir, server) = project(PROVEN);
    call(&server, "flot_run", json!({}));
    let mut dirs = vec![dir.path().join(".flot/objects")];
    while let Some(objects) = dirs.pop() {
        for entry in std::fs::read_dir(objects).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(&path, permissions).unwrap();
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.extend_from_slice(b"x\n");
            std::fs::write(&path, bytes).unwrap();
        }
    }
    let verified = call(&server, "flot_verify", json!({}));
    assert_eq!(verified["isError"], true, "{verified}");
    let audit = &verified["structuredContent"];
    assert_eq!(audit["intact"], false);
    // What the altered record says, which `result` does not repeat.
    assert_eq!(audit["verdict"], "passed");
    assert_eq!(audit["result"], "broken");
}

#[test]
fn a_run_of_some_steps_is_partial_not_passed() {
    let (_dir, server) = project(&format!(
        "{PROVEN}\n[[step]]\nid = \"other\"\nrun = \"false\"\n"
    ));
    let run = call(&server, "flot_run", json!({ "only": "write" }));
    assert_eq!(run["isError"], false, "{run}");
    assert_eq!(run["structuredContent"]["result"], "partial");
    assert_eq!(
        run["structuredContent"]["manifest"]["partial"],
        json!({ "selected": 1, "of": 2 })
    );
    let verified = call(&server, "flot_verify", json!({}));
    assert_eq!(verified["isError"], false, "{verified}");
    assert_eq!(verified["structuredContent"]["result"], "partial");
}

/// Two steps that each report 60 cents, under a flow ceiling of 50.
const CAPPED: &str = r#"
[flow]
name = "capped"
max_cost_usd = 0.5

[[step]]
id = "a"
run = '''echo '{"cost_usd": 0.6}' > "$FLOT_METRICS"'''

[[step]]
id = "b"
needs = ["a"]
run = '''echo '{"cost_usd": 0.6}' > "$FLOT_METRICS"'''
"#;

#[test]
fn flot_run_cannot_raise_the_flows_cost_ceiling() {
    let (dir, server) = project(CAPPED);
    let run = call(&server, "flot_run", json!({ "max_cost_usd": 100000 }));
    let manifest = &run["structuredContent"]["manifest"];
    assert_eq!(manifest["max_cost_micro_usd"], 500_000, "{run}");
    assert_eq!(manifest["verdict"], "failed");
    assert_eq!(manifest["steps"][1]["status"], "cancelled");
    assert_eq!(run["structuredContent"]["result"], "failed");

    // It can lower it.
    let run = call(
        &server,
        "flot_run",
        json!({ "max_cost_usd": 0.25, "force": true }),
    );
    let manifest = &run["structuredContent"]["manifest"];
    assert_eq!(manifest["max_cost_micro_usd"], 250_000, "{run}");

    // Without a ceiling in the flow, the argument is the ceiling.
    std::fs::write(
        dir.path().join("flot.toml"),
        CAPPED.replace("max_cost_usd = 0.5\n", ""),
    )
    .unwrap();
    let run = call(
        &server,
        "flot_run",
        json!({ "max_cost_usd": 100000, "force": true }),
    );
    let manifest = &run["structuredContent"]["manifest"];
    assert_eq!(manifest["max_cost_micro_usd"], 100_000_000_000u64, "{run}");
    assert_eq!(manifest["verdict"], "passed");
}

#[test]
fn a_failed_tool_reports_is_error_without_killing_the_session() {
    let (_dir, server) = project(PROVEN);
    let result = call(&server, "flot_show", json!({ "run": "nope" }));
    assert_eq!(result["isError"], true);
    assert!(result["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("no runs recorded"));

    // The server still answers afterwards.
    assert_eq!(call(&server, "flot_run", json!({}))["isError"], false);
}

#[test]
fn a_tool_error_names_its_kind() {
    let (dir, server) = project(PROVEN);
    let unknown = call(&server, "flot_show", json!({ "run": "nope" }));
    assert_eq!(unknown["structuredContent"]["kind"], "invalid");

    let run = call(&server, "flot_run", json!({}));
    let receipt_id = run["structuredContent"]["steps"][0]["receipt_id"]
        .as_str()
        .unwrap()
        .trim_start_matches("sha256:")
        .to_string();
    let path = dir.path().join(format!(".flot/receipts/{receipt_id}.json"));
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replacen("\"ok\"", "\"failed\"", 1)).unwrap();
    let tampered = call(&server, "flot_receipt", json!({ "receipt_id": receipt_id }));
    assert_eq!(tampered["isError"], true);
    assert_eq!(tampered["structuredContent"]["kind"], "integrity");
    assert!(tampered["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("does not match its body"));
}

#[test]
fn diff_over_mcp_matches_the_cli_contract() {
    let (_dir, server) = project(PROVEN);
    call(&server, "flot_run", json!({}));
    call(&server, "flot_run", json!({}));
    let runs = call(&server, "flot_runs", json!({}))["structuredContent"]["runs"].clone();
    let left = runs[0]["run_id"].as_str().unwrap().to_string();
    let right = runs[1]["run_id"].as_str().unwrap().to_string();
    let diff = call(
        &server,
        "flot_diff",
        json!({ "left": left, "right": right }),
    )["structuredContent"]
        .clone();
    assert_eq!(diff["identical"], true);
}
