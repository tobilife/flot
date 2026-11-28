//! MCP stdio server.
//!
//! The tools here are thin adapters over the same functions the CLI calls, so a
//! host and a human see one contract: identical receipts, identical verdicts,
//! identical digests. Every reply carries both a text rendering and
//! `structuredContent`, and anything bulky is returned as a digest the host can
//! fetch with `flot_read_object` rather than as an unbounded blob.

mod calls;
mod catalog;
mod jsonrpc;
#[cfg(test)]
mod tests;
mod tools;

use std::io::{BufRead, Write};
use std::sync::{mpsc, Mutex};

use anyhow::Result;
use serde_json::{json, Value};

use crate::service::{StopSignal, Workspace};

use calls::Calls;
use catalog::tool_catalog;
use jsonrpc::{error_response, parse, success, write_message};

/// Protocol revisions this server speaks. The first is preferred.
const SUPPORTED_PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

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
