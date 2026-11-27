//! MCP stdio server.
//!
//! The tools here are thin adapters over the same functions the CLI calls, so a
//! host and a human see one contract: identical receipts, identical verdicts,
//! identical digests. Every reply carries both a text rendering and
//! `structuredContent`, and anything bulky is returned as a digest the host can
//! fetch with `flot_read_object` rather than as an unbounded blob.

use std::io::{BufRead, Write};
use std::sync::{mpsc, Mutex};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};

use crate::service::audit::Outcome;
use crate::service::{RunOptions, StopSignal, Workspace};

/// Protocol revisions this server speaks. The first is preferred.
const SUPPORTED_PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Default ceiling on object bytes returned inline through MCP.
const DEFAULT_OBJECT_BYTES: usize = 16_384;

pub struct Server {
    workspace: Workspace,
}

impl Server {
    pub fn new(workspace: Workspace) -> Self {
        Server { workspace }
    }

    /// Serve newline-delimited JSON-RPC until stdin closes.
    ///
    /// Tool calls run one at a time on a worker, so the reader stays free to
    /// answer `ping` and to act on `notifications/cancelled` while a long run
    /// is in progress. A cancelled call stops its steps, records the run, and
    /// gets no reply, as MCP asks.
    pub fn serve(&self, mut input: impl BufRead, output: impl Write + Send) -> Result<()> {
        let output = Mutex::new(output);
        let calls = Calls::default();
        let (queue, work) = mpsc::channel::<(Value, Value)>();
        std::thread::scope(|scope| -> Result<()> {
            let worker = scope.spawn(|| -> Result<()> {
                for (id, params) in work {
                    let Some(stop) = calls.start(&id) else {
                        continue;
                    };
                    let response = self.call(id.clone(), &params, &stop);
                    if calls.finish(&id) {
                        write_message(&output, &response)?;
                    }
                    // flot was asked to stop: the run is recorded and
                    // answered, and the reader may be blocked on stdin.
                    if crate::service::interrupted() {
                        std::process::exit(130);
                    }
                }
                Ok(())
            });
            let read = self.read_loop(&mut input, &output, &calls, &queue);
            drop(queue);
            let worked = worker.join().expect("the MCP worker does not panic");
            read.and(worked)
        })
    }

    fn read_loop(
        &self,
        input: &mut impl BufRead,
        output: &Mutex<impl Write>,
        calls: &Calls,
        queue: &mpsc::Sender<(Value, Value)>,
    ) -> Result<()> {
        let mut line = Vec::new();
        loop {
            line.clear();
            if input.read_until(b'\n', &mut line)? == 0 {
                return Ok(());
            }
            let reply = match parse(&line) {
                Err(reply) => Some(reply),
                Ok(None) => None,
                Ok(Some(message)) => match message.method.as_str() {
                    "tools/call" => {
                        calls.queue(&message.id);
                        queue
                            .send((message.id, message.params))
                            .expect("the MCP worker outlives the reader");
                        None
                    }
                    "notifications/cancelled" => {
                        if let Some(id) = message.params.get("requestId") {
                            calls.cancel(id);
                        }
                        None
                    }
                    _ => self.answer(&message),
                },
            };
            if let Some(reply) = reply {
                write_message(output, &reply)?;
            }
            // A run in progress was stopped and recorded; the server stops too.
            if crate::service::interrupted() {
                return Ok(());
            }
        }
    }

    /// Answer one message. Notifications return `None`. Tool calls are
    /// answered in place here; `serve` runs them on its worker instead.
    pub fn handle(&self, message: &Value) -> Option<Value> {
        let line = serde_json::to_vec(message).expect("a JSON value serializes");
        match parse(&line) {
            Err(reply) => Some(reply),
            Ok(None) => None,
            Ok(Some(message)) if message.method == "tools/call" => {
                Some(self.call(message.id, &message.params, &StopSignal::default()))
            }
            Ok(Some(message)) => self.answer(&message),
        }
    }

    fn answer(&self, message: &Request) -> Option<Value> {
        let id = message.id.clone();
        match message.method.as_str() {
            "initialize" => Some(success(id, self.initialize(&message.params))),
            "ping" => Some(success(id, json!({}))),
            "tools/list" => Some(success(id, json!({ "tools": tool_catalog() }))),
            // Notifications never get a reply, known or not.
            method if method.starts_with("notifications/") => None,
            other => Some(error_response(
                id,
                -32601,
                &format!("unknown method `{other}`"),
            )),
        }
    }

    fn initialize(&self, params: &Value) -> Value {
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(SUPPORTED_PROTOCOLS[0]);
        let protocol = if SUPPORTED_PROTOCOLS.contains(&requested) {
            requested
        } else {
            SUPPORTED_PROTOCOLS[0]
        };
        json!({
            "protocolVersion": protocol,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": {
                "name": "flot",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": "Run declared flows and read their receipts. A claim is unverified until its verify command exits zero, so treat any run whose verdict is not `passed` as unproven. Inline output is budgeted; fetch complete streams with flot_read_object using the digest from a receipt."
        })
    }

    fn call(&self, id: Value, params: &Value, stop: &StopSignal) -> Value {
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
                    match (flag("stderr"), flag("proof"), flag("held")) {
                        (false, false, false) => crate::service::Stream::Stdout,
                        (true, false, false) => crate::service::Stream::Stderr,
                        (false, true, false) => crate::service::Stream::Proof,
                        (false, false, true) => crate::service::Stream::Held,
                        _ => bail!(
                            "`stderr`, `proof` and `held` name different outputs; ask for one"
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

/// Serve MCP on this process's stdio until stdin closes.
pub fn serve_stdio(workspace: &Workspace) -> Result<()> {
    let stdin = std::io::stdin();
    Server::new(workspace.clone()).serve(stdin.lock(), std::io::stdout())
}

/// A JSON-RPC message that asks for something: a request (with an id) or a
/// notification (without one; `id` is then null and nothing is sent back).
struct Request {
    id: Value,
    method: String,
    params: Value,
}

/// Read one line. `Ok(None)` is a line that needs no reply: blank, a
/// notification, or a response to a request this server never sends. `Err`
/// is the error reply for a line that is not a valid request.
fn parse(line: &[u8]) -> std::result::Result<Option<Request>, Value> {
    let invalid = |code: i32, message: &str| error_response(Value::Null, code, message);
    let Ok(text) = std::str::from_utf8(line) else {
        return Err(invalid(-32700, "parse error: the line is not UTF-8"));
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let message: Value = serde_json::from_str(text)
        .map_err(|error| invalid(-32700, &format!("parse error: {error}")))?;
    // MCP 2025-06-18 removed JSON-RPC batching. Saying so beats silence,
    // which a client would wait on forever.
    let Some(object) = message.as_object() else {
        return Err(invalid(
            -32600,
            if message.is_array() {
                "batch requests are not supported; send one message per line"
            } else {
                "a request must be a JSON object"
            },
        ));
    };
    let id = object.get("id");
    let method = object.get("method");
    if method.is_none() && (object.contains_key("result") || object.contains_key("error")) {
        return Ok(None);
    }
    let reply_id = match id {
        Some(id) if id.is_string() || id.is_number() => id.clone(),
        _ => Value::Null,
    };
    let Some(method) = method.and_then(Value::as_str) else {
        return Err(error_response(
            reply_id,
            -32600,
            "a request needs a string `method`",
        ));
    };
    let params = object.get("params").cloned().unwrap_or(json!({}));
    match id {
        None => Ok(method.starts_with("notifications/").then(|| Request {
            id: Value::Null,
            method: method.to_string(),
            params,
        })),
        Some(id) if id.is_string() || id.is_number() => Ok(Some(Request {
            id: id.clone(),
            method: method.to_string(),
            params,
        })),
        Some(_) => Err(invalid(-32600, "a request id must be a string or a number")),
    }
}

/// Tool calls the worker has been given: queued, running, or withdrawn.
#[derive(Default)]
struct Calls {
    state: Mutex<CallState>,
}

#[derive(Default)]
struct CallState {
    queued: Vec<Value>,
    running: Option<(Value, StopSignal)>,
    withdrawn: Vec<Value>,
}

impl Calls {
    fn queue(&self, id: &Value) {
        self.lock().queued.push(id.clone());
    }

    /// The call's stop signal, unless it was withdrawn while it waited.
    fn start(&self, id: &Value) -> Option<StopSignal> {
        let mut state = self.lock();
        state.queued.retain(|queued| queued != id);
        if let Some(position) = state.withdrawn.iter().position(|withdrawn| withdrawn == id) {
            state.withdrawn.remove(position);
            return None;
        }
        let stop = StopSignal::default();
        state.running = Some((id.clone(), stop.clone()));
        Some(stop)
    }

    /// Whether the finished call still wants its reply.
    fn finish(&self, id: &Value) -> bool {
        let mut state = self.lock();
        state.running = None;
        match state.withdrawn.iter().position(|withdrawn| withdrawn == id) {
            Some(position) => {
                state.withdrawn.remove(position);
                false
            }
            None => true,
        }
    }

    fn cancel(&self, id: &Value) {
        let mut state = self.lock();
        if let Some((_, stop)) = state.running.as_ref().filter(|(running, _)| running == id) {
            stop.stop();
            state.withdrawn.push(id.clone());
        } else if state.queued.contains(id) {
            state.withdrawn.push(id.clone());
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CallState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Check tool arguments against the schema the catalog advertises, so what a
/// host was told is what is enforced: an object, no unknown keys, required
/// keys present, declared types and minimums.
fn check_arguments(schema: &Value, arguments: &Value) -> std::result::Result<(), String> {
    let Some(arguments) = arguments.as_object() else {
        return Err("`arguments` must be an object".into());
    };
    let properties = schema["properties"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for (key, value) in arguments {
        let Some(property) = properties.get(key) else {
            return Err(format!("unknown argument `{key}`"));
        };
        let minimum = property.get("minimum").and_then(Value::as_f64);
        let fits = match property["type"].as_str() {
            Some("string") => value.is_string(),
            Some("boolean") => value.is_boolean(),
            Some("integer") => value
                .as_u64()
                .is_some_and(|number| minimum.is_none_or(|minimum| number as f64 >= minimum)),
            Some("number") => value.as_f64().is_some_and(|number| {
                number.is_finite() && minimum.is_none_or(|minimum| number >= minimum)
            }),
            _ => true,
        };
        if !fits {
            let kind = property["type"].as_str().unwrap_or("value");
            return Err(match minimum {
                Some(minimum) => format!("`{key}` must be a {kind} of at least {minimum}"),
                None => format!("`{key}` must be a {kind}"),
            });
        }
    }
    for key in schema["required"].as_array().into_iter().flatten() {
        let key = key.as_str().unwrap_or_default();
        if !arguments.contains_key(key) {
            return Err(format!("`{key}` is required"));
        }
    }
    Ok(())
}

fn tool_result(payload: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": payload,
        "isError": is_error,
    })
}

fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn write_message(output: &Mutex<impl Write>, message: &Value) -> Result<()> {
    let mut output = output
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    writeln!(output, "{}", serde_json::to_string(message)?)?;
    output.flush()?;
    Ok(())
}

/// Whether a tool only reads the store, or runs the flow's commands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    Reads,
    Runs,
}

fn tool(
    name: &str,
    effect: Effect,
    description: &str,
    properties: Value,
    required: &[&str],
) -> Value {
    let mut schema = Map::new();
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), properties);
    schema.insert("required".into(), json!(required));
    schema.insert("additionalProperties".into(), json!(false));
    // Hosts use these hints to decide what needs a person's approval. Running
    // a flow executes arbitrary, unsandboxed commands, so it claims nothing
    // reassuring; reading the store changes nothing and reaches nothing.
    let annotations = match effect {
        Effect::Reads => json!({
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        }),
        Effect::Runs => json!({
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": true,
        }),
    };
    json!({
        "name": name,
        "description": description,
        "inputSchema": Value::Object(schema),
        "annotations": annotations,
    })
}

fn tool_catalog() -> Vec<Value> {
    vec![
        tool(
            "flot_plan",
            Effect::Reads,
            "Show what the flow would run, what it would reuse from receipts, and which claims have no proof command. Runs nothing. Reuse after an `always` step is predicted on that step coming out as it did last time; such a step lists those steps in `assumes`.",
            json!({
                "only": { "type": "string", "description": "Limit to one step and its dependencies." },
                "force": { "type": "boolean", "description": "Ignore cached receipts." }
            }),
            &[],
        ),
        tool(
            "flot_run",
            Effect::Runs,
            "Execute the flow and record a receipt per step. Act on `result`: `passed` only when every step of the flow succeeded and every claim was proven; `partial` when every selected step passed but `only` left steps out, so nothing is known of those (the manifest's `partial` gives `selected` and `of`; its `verdict` still reads `passed`, which is not a pass of the flow); `unverified` when work completed but a claim has no proof; `failed` when a step failed or a claim was refuted. A step with `cached: true` was not executed again because the same command already succeeded over byte-identical inputs and upstream receipts, and every file it created, edited or deleted is still as it left it (its outputs are put back); its reused receipt is as strong as a fresh run, so do not force a re-run to double-check it. A reused step whose manifest entry has `vouched_by` did not run on this host: its receipt came from an imported run, and `vouched` lists each such step's signers with `confirmed` (this store holds a run the key signed that kept the same receipt) and `trusted_as` (the name this host trusts a confirmed key under). A step's inline output keeps its first and last lines; flot_step_output returns all of it.",
            json!({
                "only": { "type": "string", "description": "Limit to one step and its dependencies." },
                "force": { "type": "boolean", "description": "Execute every selected step even when an identical receipt could be reused; for steps that are flaky or depend on something undeclared." },
                "jobs": { "type": "integer", "minimum": 1, "description": "Run up to this many independent steps at once." },
                "max_cost_usd": { "type": "number", "minimum": 0, "description": "Stop starting new steps once executed steps have reported this many dollars through $FLOT_METRICS. It can only lower the flow's own `max_cost_usd`: when the flow sets one, the lower of the two applies." }
            }),
            &[],
        ),
        tool(
            "flot_show",
            Effect::Reads,
            "Summarize a recorded run: steps, claim verdicts and limitations. Accepts `latest` (the newest run executed here), a run id, or a prefix of at least 4 characters of a run id or its root.",
            json!({ "run": { "type": "string", "description": "Run selector; defaults to latest." } }),
            &[],
        ),
        tool(
            "flot_verify",
            Effect::Reads,
            "Re-authenticate a recorded run from the store alone: receipt bodies against their ids, dependency references, every evidence object re-read against its digest, and the run root. Act on `result`, the worst that holds, which is also what `flot verify` exits with: `broken` (exit 1, and `isError`) when the evidence does not authenticate, as `findings` says, and `verdict` is then only what the altered record claims; `failed` (exit 1) when a step failed or a claim was refuted; `unverified` (exit 2) when a claim has no proof or the proofs differ from the revision in `anchor`; `partial` (exit 2) when every selected step passed but the run left steps of the flow out, as `partial` gives `selected` of `of`; `passed` (exit 0) only otherwise. `authorship` says who vouched for the run, judged against this host's trust list now: `unsigned`; `untrusted` (validly signed by no key this host trusts); `this_host` (signed by this host's own key, which is not on its trust list: a label, not trust: anything running here as this user, an agent included, can sign with it); `trusted` (validly signed by a key on the trust list, named in `signers`). `vouched` lists the steps the run reused from imported runs, each with its signers marked `confirmed` (this store holds a run the key signed that kept the same receipt) and `trusted_as`; a signer not confirmed is a name the run gives, not a signature.",
            json!({ "run": { "type": "string", "description": "Run selector; defaults to latest." } }),
            &[],
        ),
        tool(
            "flot_diff",
            Effect::Reads,
            "Compare two runs by receipt identity. Identical receipts mean the same work over the same inputs reached the same verdict. Each changed step lists in `reasons` why, read from its two receipts: the files it or its proof read, its dependencies, its command, its claims, or, with the same key, the same work coming out otherwise.",
            json!({
                "left": { "type": "string", "description": "Run selector; defaults to latest~1, the run before the latest." },
                "right": { "type": "string", "description": "Run selector; defaults to latest." }
            }),
            &[],
        ),
        tool(
            "flot_receipt",
            Effect::Reads,
            "Read one receipt as structured data. Accepts a full receipt id or an unambiguous hex prefix.",
            json!({ "receipt_id": { "type": "string", "description": "Receipt id or hex prefix." } }),
            &["receipt_id"],
        ),
        tool(
            "flot_read_object",
            Effect::Reads,
            "Read a stored object, such as a step's complete stdout, by digest. Use the digest from a receipt when its inline preview was trimmed.",
            json!({
                "digest": { "type": "string", "description": "sha256:<hex> digest from a receipt." },
                "max_bytes": { "type": "integer", "minimum": 1, "description": "Inline byte ceiling; the complete object stays retrievable." }
            }),
            &["digest"],
        ),
        tool(
            "flot_step_output",
            Effect::Reads,
            "Read the complete output one step of a run printed, such as the failing test log behind a trimmed preview, or with `proof` what its proof printed, such as why a claim was refuted. By default the receipt the run kept; `candidate` and `attempt` read a candidate it did not keep or an attempt that did not hold.",
            json!({
                "step": { "type": "string", "description": "Step id." },
                "candidate": { "type": "string", "description": "Another candidate the run executed; defaults to the kept one." },
                "attempt": { "type": "integer", "minimum": 1, "description": "Attempt number from 1; defaults to the last." },
                "run": { "type": "string", "description": "Run selector; defaults to latest." },
                "stderr": { "type": "boolean", "description": "Read stderr instead of stdout." },
                "proof": { "type": "boolean", "description": "Read what the step's proof printed (stdout and stderr together) instead." },
                "held": { "type": "boolean", "description": "Read what the step's proof printed with its held files as the revision has them instead." },
                "max_bytes": { "type": "integer", "minimum": 1, "description": "Inline byte ceiling; the complete object stays retrievable." }
            }),
            &["step"],
        ),
        tool(
            "flot_runs",
            Effect::Reads,
            "List recorded runs, newest first, with their verdicts, root digests and whether they were imported; runs that cannot be read are named under `unreadable`.",
            json!({ "limit": { "type": "integer", "minimum": 1, "description": "Maximum runs to return." } }),
            &[],
        ),
        tool(
            "flot_search",
            Effect::Reads,
            "Find text in recorded runs, newest first: each executed step's complete stdout and stderr, its proofs' output, its claims and their reasons, and limitations, including attempts and candidates that were not kept. The match is literal and ignores case. Use it to find when an error first appeared or which run a message came from; read a whole output with flot_step_output.",
            json!({
                "query": { "type": "string", "description": "Text to find." },
                "runs": { "type": "integer", "minimum": 1, "description": "How many of the newest runs to search; defaults to 50." },
                "limit": { "type": "integer", "minimum": 1, "description": "Stop after this many hits; defaults to 50." }
            }),
            &["query"],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn initialize_accepts_a_supported_protocol_and_names_the_server() {
        let (_dir, server) = project(PROVEN);
        let response = server
            .handle(&request(
                "initialize",
                json!({ "protocolVersion": "2024-11-05" }),
            ))
            .unwrap();
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(response["result"]["serverInfo"]["name"], "flot");
        assert!(response["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn an_unknown_protocol_falls_back_to_the_preferred_revision() {
        let (_dir, server) = project(PROVEN);
        let response = server
            .handle(&request(
                "initialize",
                json!({ "protocolVersion": "1999-01-01" }),
            ))
            .unwrap();
        assert_eq!(
            response["result"]["protocolVersion"],
            SUPPORTED_PROTOCOLS[0]
        );
    }

    #[test]
    fn notifications_get_no_reply() {
        let (_dir, server) = project(PROVEN);
        assert!(server
            .handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .is_none());
    }

    #[test]
    fn a_steps_output_is_read_by_step_id_without_a_digest() {
        let (_dir, server) = project(PROVEN);
        call(&server, "flot_run", json!({}));
        let output = call(&server, "flot_step_output", json!({ "step": "write" }));
        assert_eq!(output["isError"], false, "{output}");
        assert_eq!(output["structuredContent"]["text"], "flot\n");
        let missing = call(&server, "flot_step_output", json!({ "step": "read" }));
        assert_eq!(missing["isError"], true, "{missing}");
    }

    #[test]
    fn a_steps_proof_output_is_read_with_proof() {
        let (_dir, server) = project(&PROVEN.replace(
            "verify = \"grep -q flot artifact.txt\"",
            "verify = \"echo proof says no; exit 1\"",
        ));
        call(&server, "flot_run", json!({}));
        let proof = call(
            &server,
            "flot_step_output",
            json!({ "step": "write", "proof": true }),
        );
        assert_eq!(proof["isError"], false, "{proof}");
        assert_eq!(proof["structuredContent"]["text"], "proof says no\n");
        let both = call(
            &server,
            "flot_step_output",
            json!({ "step": "write", "proof": true, "stderr": true }),
        );
        assert_eq!(both["isError"], true, "{both}");
        let stderr = call(
            &server,
            "flot_step_output",
            json!({ "step": "write", "stderr": true }),
        );
        assert_eq!(stderr["isError"], false, "{stderr}");
        let held = call(
            &server,
            "flot_step_output",
            json!({ "step": "write", "held": true }),
        );
        assert_eq!(held["isError"], true, "{held}");
        assert!(held.to_string().contains("holds no files"), "{held}");
    }

    #[test]
    fn search_finds_a_steps_output_line() {
        let (_dir, server) = project(PROVEN);
        call(&server, "flot_run", json!({}));
        let found = call(&server, "flot_search", json!({ "query": "FLOT" }));
        assert_eq!(found["isError"], false, "{found}");
        let hits = found["structuredContent"]["hits"].as_array().unwrap();
        assert_eq!(hits[0]["source"], "claim");
        assert_eq!(hits[0]["text"], "artifact.txt mentions flot [verified]");
        assert_eq!(hits[1]["step_id"], "write");
        assert_eq!(hits[1]["source"], "stdout");
        assert_eq!(hits[1]["line"], 1);
        assert_eq!(hits[1]["text"], "flot");
        let empty = call(&server, "flot_search", json!({ "query": "" }));
        assert_eq!(empty["isError"], true, "{empty}");
    }

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
        let skill = include_str!("../../plugins/claude-code/skills/flot/SKILL.md");
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
    fn an_unknown_method_is_a_jsonrpc_error() {
        let (_dir, server) = project(PROVEN);
        let response = server.handle(&request("tools/nope", json!({}))).unwrap();
        assert_eq!(response["error"]["code"], -32601);
    }

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
    fn a_receipt_prefix_resolves_and_its_output_object_is_readable() {
        let (_dir, server) = project(PROVEN);
        let run = call(&server, "flot_run", json!({}));
        let receipt_id = run["structuredContent"]["steps"][0]["receipt_id"]
            .as_str()
            .unwrap()
            .to_string();
        let prefix: String = receipt_id
            .trim_start_matches("sha256:")
            .chars()
            .take(10)
            .collect();

        let receipt = call(&server, "flot_receipt", json!({ "receipt_id": prefix }))
            ["structuredContent"]
            .clone();
        assert_eq!(receipt["receipt_id"], receipt_id);

        let digest = receipt["identity"]["stdout_digest"].as_str().unwrap();
        let object = call(&server, "flot_read_object", json!({ "digest": digest }))
            ["structuredContent"]
            .clone();
        assert_eq!(object["truncated"], false);
        assert_eq!(object["text"], "flot\n");
    }

    #[test]
    fn a_trimmed_object_states_how_to_read_the_rest() {
        let (_dir, server) = project(
            r#"
[flow]
name = "chatty"

[[step]]
id = "chatty"
run = "yes flot | head -c 4000"
"#,
        );
        let run = call(&server, "flot_run", json!({}));
        let receipt_id = run["structuredContent"]["steps"][0]["receipt_id"]
            .as_str()
            .unwrap()
            .to_string();
        let receipt = call(&server, "flot_receipt", json!({ "receipt_id": receipt_id }))
            ["structuredContent"]
            .clone();
        let digest = receipt["identity"]["stdout_digest"].as_str().unwrap();
        let object = call(
            &server,
            "flot_read_object",
            json!({ "digest": digest, "max_bytes": 100 }),
        )["structuredContent"]
            .clone();
        assert_eq!(object["truncated"], true);
        assert_eq!(object["bytes"], 4000);
        assert!(object["recovery"].as_str().unwrap().contains("4000"));
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
    fn an_unknown_tool_or_a_bad_argument_is_an_invalid_params_error() {
        let (_dir, server) = project(PROVEN);
        let invalid = |params: Value| {
            let response = server.handle(&request("tools/call", params)).unwrap();
            assert_eq!(response["error"]["code"], -32602, "{response}");
            response["error"]["message"].as_str().unwrap().to_string()
        };
        assert!(invalid(json!({ "name": "flot_teleport" })).contains("unknown tool"));
        assert!(invalid(json!({ "arguments": {} })).contains("`name`"));
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
        ] {
            let message = invalid(json!({ "name": name, "arguments": arguments }));
            assert!(message.contains(expected), "{name}: {message}");
        }
        // Nothing ran on any of those.
        assert!(!_dir.path().join(".flot").exists());
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

    /// Feed `input` to `serve` and return its replies.
    fn served(server: &Server, input: &[u8]) -> Vec<Value> {
        let mut output = Vec::new();
        server
            .serve(std::io::BufReader::new(input), &mut output)
            .unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn malformed_envelopes_get_an_error_and_the_stream_continues() {
        let (_dir, server) = project(PROVEN);
        let mut input = b"\xff\xfe\n".to_vec();
        input.extend_from_slice(
            concat!(
                "{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"ping\"}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":3}\n",
                "{}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{}}\n",
                "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n",
            )
            .as_bytes(),
        );
        let replies = served(&server, &input);
        let codes: Vec<Option<i64>> = replies
            .iter()
            .map(|reply| reply["error"]["code"].as_i64())
            .collect();
        assert_eq!(
            codes,
            [Some(-32700), Some(-32600), Some(-32600), Some(-32600), None]
        );
        assert_eq!(replies[2]["id"], 3, "an id that can be read is echoed");
        assert_eq!(replies[4]["id"], 7);
    }

    /// Lines written by `serve`, delivered as they are written.
    struct Replies(mpsc::Sender<Value>, Vec<u8>);

    impl Write for Replies {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.1.extend_from_slice(bytes);
            while let Some(end) = self.1.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.1.drain(..=end).collect();
                let _ = self.0.send(serde_json::from_slice(&line).unwrap());
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_long_call_still_answers_ping_and_a_cancelled_one_stops_and_is_recorded() {
        let (dir, server) =
            project("[flow]\nname = \"slow\"\n\n[[step]]\nid = \"wait\"\nrun = \"sleep 30\"\n");
        let (reader, mut writer) = std::io::pipe().unwrap();
        let (sender, replies) = mpsc::channel();
        let started = std::time::Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                server
                    .serve(std::io::BufReader::new(reader), Replies(sender, Vec::new()))
                    .unwrap()
            });
            let send = |writer: &mut std::io::PipeWriter, message: Value| {
                writeln!(writer, "{message}").unwrap();
            };
            send(
                &mut writer,
                request("tools/call", json!({ "name": "flot_run", "arguments": {} })),
            );
            std::thread::sleep(std::time::Duration::from_millis(300));
            send(
                &mut writer,
                json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }),
            );
            let pong = replies
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("ping is answered while the run is in progress");
            assert_eq!(pong["id"], 2);
            send(
                &mut writer,
                json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 1 } }),
            );
            drop(writer);
        });
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
        assert!(
            replies.try_recv().is_err(),
            "a cancelled call gets no reply"
        );

        // The stopped run is recorded, and the next call is not cancelled.
        let runs = call(&server, "flot_runs", json!({}))["structuredContent"]["runs"].clone();
        assert_eq!(runs.as_array().unwrap().len(), 1);
        assert_eq!(runs[0]["verdict"], "failed");
        let show = call(&server, "flot_show", json!({}))["structuredContent"].clone();
        assert!(
            show["limitations"].to_string().contains("asked to stop"),
            "{show}"
        );
        std::fs::write(
            dir.path().join("flot.toml"),
            "[flow]\nname = \"quick\"\n\n[[step]]\nid = \"now\"\nrun = \"true\"\n",
        )
        .unwrap();
        let next = call(&server, "flot_run", json!({}));
        assert_eq!(
            next["structuredContent"]["steps"][0]["status"], "ok",
            "{next}"
        );
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

    #[test]
    fn a_malformed_line_gets_a_parse_error_and_the_stream_continues() {
        let (_dir, server) = project(PROVEN);
        let input = "not json\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n";
        let mut output = Vec::new();
        server
            .serve(std::io::BufReader::new(input.as_bytes()), &mut output)
            .unwrap();
        let lines: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["error"]["code"], -32700);
        assert_eq!(lines[1]["id"], 7);
    }

    #[test]
    fn a_batch_or_non_object_gets_an_invalid_request_instead_of_silence() {
        let (_dir, server) = project(PROVEN);
        let input = "[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]\n42\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n";
        let mut output = Vec::new();
        server
            .serve(std::io::BufReader::new(input.as_bytes()), &mut output)
            .unwrap();
        let lines: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["error"]["code"], -32600);
        assert!(lines[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("batch"));
        assert_eq!(lines[1]["error"]["code"], -32600);
        assert_eq!(lines[2]["id"], 7);
    }
}
