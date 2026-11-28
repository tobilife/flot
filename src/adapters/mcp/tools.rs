//! Answering a tool call: its arguments checked against the catalog, the
//! workspace function it names called, and the reply.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::service::audit::Outcome;
use crate::service::{RunOptions, StopSignal};

use super::catalog::{check_arguments, tool_catalog};
use super::jsonrpc::{error_response, success};
use super::Server;

/// Default ceiling on object bytes returned inline through MCP.
const DEFAULT_OBJECT_BYTES: usize = 16_384;

impl Server {
    pub(super) fn call(&self, id: Value, params: &Value, stop: &StopSignal) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error_response(id, -32602, "tools/call needs a string `name`");
        };
        let Some(schema) = tool_catalog()
            .into_iter()
            .find(|tool| tool["name"] == name)
            .map(|tool| tool["inputSchema"].clone())
        else {
            return error_response(id, -32602, &format!("unknown tool `{name}`"));
        };
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        if let Err(problem) = check_arguments(&schema, &arguments) {
            return error_response(id, -32602, &format!("{name}: {problem}"));
        }
        match self.invoke(name, &arguments, stop) {
            Ok((result, is_error)) => success(id, tool_result(&result, is_error)),
            Err(error) => success(
                id,
                tool_result(
                    &json!({
                        "error": format!("{error:#}"),
                        "kind": crate::service::fault_kind(&error),
                    }),
                    true,
                ),
            ),
        }
    }

    /// A tool's reply, and whether it reports an error: evidence that does
    /// not authenticate is one, as it is to the CLI.
    fn invoke(&self, name: &str, arguments: &Value, stop: &StopSignal) -> Result<(Value, bool)> {
        let text = |key: &str| {
            arguments
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let required = |key: &str| text(key).ok_or_else(|| anyhow!("`{key}` is required"));
        let run_selector = || text("run").unwrap_or_else(|| "latest".into());
        let flag = |key: &str| arguments.get(key).and_then(Value::as_bool).unwrap_or(false);
        let count = |key: &str| {
            arguments
                .get(key)
                .and_then(Value::as_u64)
                .map(|value| value as usize)
        };
        let workspace = &self.workspace;
        let mut is_error = false;

        let value = match name {
            "flot_plan" => serde_json::to_value(workspace.plan(&RunOptions {
                only: text("only"),
                force: flag("force"),
                ..RunOptions::default()
            })?)?,
            "flot_run" => {
                // The flow's ceiling is the user's, in a file the guard keeps
                // agents from editing: the agent calling the tool may only
                // lower it.
                let asked = arguments.get("max_cost_usd").and_then(Value::as_f64);
                let report = workspace.run(&RunOptions {
                    only: text("only"),
                    force: flag("force"),
                    jobs: count("jobs"),
                    max_cost_usd: match (asked, workspace.flow()?.meta.max_cost_usd) {
                        (Some(asked), Some(ceiling)) => Some(asked.min(ceiling)),
                        (asked, _) => asked,
                    },
                    stop: stop.clone(),
                    // Not an argument: the agent calling the tool must not choose
                    // the revision its own proofs are checked against.
                    proofs_from: None,
                    // A tool call answers once, with the run; nothing streams.
                    progress: Default::default(),
                })?;
                let mut value = serde_json::to_value(&report)?;
                value["result"] = serde_json::to_value(Outcome::of_run(&report.manifest))?;
                value
            }
            "flot_show" => serde_json::to_value(workspace.show(&run_selector())?)?,
            "flot_verify" => {
                let audit = workspace.verify(&run_selector(), None)?;
                is_error = audit.result() == Outcome::Broken;
                serde_json::to_value(audit)?
            }
            "flot_diff" => serde_json::to_value(workspace.diff(
                &text("left").unwrap_or_else(|| "latest~1".into()),
                &text("right").unwrap_or_else(|| "latest".into()),
            )?)?,
            "flot_receipt" => serde_json::to_value(workspace.receipt(&required("receipt_id")?)?)?,
            "flot_read_object" => serde_json::to_value(workspace.read_object(
                &required("digest")?,
                count("max_bytes").unwrap_or(DEFAULT_OBJECT_BYTES),
            )?)?,
            "flot_step_output" => serde_json::to_value(workspace.read_object(
                &workspace.step_output(
                    &run_selector(),
                    Some(&required("step")?),
                    &crate::service::OutputOf {
                        candidate: text("candidate"),
                        attempt: count("attempt"),
                    },
                    match (flag("stderr"), flag("proof"), flag("held"), flag("mutants")) {
                        (false, false, false, false) => crate::service::Stream::Stdout,
                        (true, false, false, false) => crate::service::Stream::Stderr,
                        (false, true, false, false) => crate::service::Stream::Proof,
                        (false, false, true, false) => crate::service::Stream::Held,
                        (false, false, false, true) => crate::service::Stream::Mutants,
                        _ => bail!(
                            "`stderr`, `proof`, `held` and `mutants` name different outputs; ask for one"
                        ),
                    },
                )?,
                count("max_bytes").unwrap_or(DEFAULT_OBJECT_BYTES),
            )?)?,
            "flot_runs" => serde_json::to_value(workspace.runs(count("limit").unwrap_or(20))?)?,
            "flot_search" => serde_json::to_value(workspace.search(
                &required("query")?,
                count("runs").unwrap_or(50),
                count("limit").unwrap_or(50),
            )?)?,
            other => unreachable!("`call` admits only catalog tools, not `{other}`"),
        };
        Ok((value, is_error))
    }
}

fn tool_result(payload: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": payload,
        "isError": is_error,
    })
}
