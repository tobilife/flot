//! The tool catalog: what each tool declares, and the arguments a call is
//! held to.

use serde_json::{json, Value};

use super::{project, request, PROVEN};

#[test]
fn every_catalog_tool_declares_an_object_schema() {
    let (_dir, server) = project(PROVEN);
    let response = server.handle(&request("tools/list", json!({}))).unwrap();
    let tools = response["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 10);
    for entry in tools {
        assert!(entry["name"].as_str().unwrap().starts_with("flot_"));
        assert!(!entry["description"].as_str().unwrap().is_empty());
        assert_eq!(entry["inputSchema"]["type"], "object");
    }
}

#[test]
fn every_tool_the_plugin_skill_names_is_one_the_server_lists() {
    let (_dir, server) = project(PROVEN);
    let response = server.handle(&request("tools/list", json!({}))).unwrap();
    let listed: Vec<&str> = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    let skill = include_str!("../../../../plugins/claude-code/skills/flot/SKILL.md");
    let named: Vec<&str> = skill
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| word.starts_with("flot_"))
        .collect();
    assert!(named.contains(&"flot_run"), "{named:?}");
    for name in named {
        assert!(
            listed.contains(&name),
            "the skill names {name}, which is not a tool"
        );
    }
}

#[test]
fn only_running_the_flow_is_announced_as_having_effects() {
    let (_dir, server) = project(PROVEN);
    let response = server.handle(&request("tools/list", json!({}))).unwrap();
    for entry in response["result"]["tools"].as_array().unwrap() {
        let runs = entry["name"] == "flot_run";
        let hints = &entry["annotations"];
        assert_eq!(hints["readOnlyHint"], !runs, "{}", entry["name"]);
        assert_eq!(hints["destructiveHint"], runs, "{}", entry["name"]);
        assert_eq!(hints["openWorldHint"], runs, "{}", entry["name"]);
    }
}

#[test]
fn an_unknown_tool_or_a_bad_argument_is_an_invalid_params_error() {
    let (_dir, server) = project(PROVEN);
    let invalid = |params: Value| {
        let response = server.handle(&request("tools/call", params)).unwrap();
        assert_eq!(response["error"]["code"], -32602, "{response}");
        response["error"]["message"].as_str().unwrap().to_string()
    };
    assert!(invalid(json!({ "name": "flot_teleport" })).contains("unknown tool"));
    assert!(invalid(json!({ "arguments": {} })).contains("`name`"));
    // Parameters that are not an object name no tool.
    for params in [
        json!(5),
        json!("flot_run"),
        json!(["flot_run"]),
        Value::Null,
    ] {
        assert!(invalid(params.clone()).contains("`name`"), "{params}");
    }
    for (name, arguments, expected) in [
        (
            "flot_run",
            json!({ "only": ["write"] }),
            "`only` must be a string",
        ),
        (
            "flot_run",
            json!({ "step": "write" }),
            "unknown argument `step`",
        ),
        (
            "flot_run",
            json!({ "force": "true" }),
            "`force` must be a boolean",
        ),
        ("flot_run", json!({ "jobs": 0 }), "at least 1"),
        (
            "flot_run",
            json!({ "jobs": 2.5 }),
            "`jobs` must be a integer",
        ),
        (
            "flot_run",
            json!({ "max_cost_usd": "0.01" }),
            "`max_cost_usd` must be a number",
        ),
        ("flot_run", json!({ "max_cost_usd": -1 }), "at least 0"),
        ("flot_show", json!({ "run": 123 }), "`run` must be a string"),
        ("flot_diff", json!({ "left": 5 }), "`left` must be a string"),
        ("flot_receipt", json!({}), "`receipt_id` is required"),
        ("flot_runs", json!("everything"), "must be an object"),
        ("flot_runs", Value::Null, "must be an object"),
        // The revision a run's proofs are held to is the flow's, never the
        // calling agent's.
        (
            "flot_run",
            json!({ "proofs_from": "HEAD" }),
            "unknown argument `proofs_from`",
        ),
    ] {
        let message = invalid(json!({ "name": name, "arguments": arguments }));
        assert!(message.contains(expected), "{name}: {message}");
    }
    // Nothing ran on any of those.
    assert!(!_dir.path().join(".flot").exists());
}

/// An agent may lower the flow's cost ceiling, never raise it; with no
/// ceiling of the flow's, what it asks for is the ceiling.
#[test]
fn a_run_called_through_mcp_never_spends_past_the_flows_ceiling() {
    use super::super::tools::capped;
    assert_eq!(capped(Some(100_000.0), Some(0.5)), Some(0.5));
    assert_eq!(capped(Some(0.5), Some(0.5)), Some(0.5));
    assert_eq!(capped(Some(0.25), Some(0.5)), Some(0.25));
    assert_eq!(capped(Some(0.0), Some(0.5)), Some(0.0));
    assert_eq!(capped(Some(3.0), None), Some(3.0));
    assert_eq!(capped(None, Some(0.5)), None);
    assert_eq!(capped(None, None), None);
}
