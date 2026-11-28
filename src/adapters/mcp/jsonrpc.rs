//! JSON-RPC over newline-delimited stdio: one line read into a request,
//! and the replies written back.

use std::io::Write;
use std::sync::Mutex;

use anyhow::Result;
use serde_json::{json, Value};

use super::Request;

/// Read one line. `Ok(None)` is a line that needs no reply: blank, a
/// notification, or a response to a request this server never sends. `Err`
/// is the error reply for a line that is not a valid request.
pub(super) fn parse(line: &[u8]) -> std::result::Result<Option<Request>, Value> {
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

pub(super) fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub(super) fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub(super) fn write_message(output: &Mutex<impl Write>, message: &Value) -> Result<()> {
    let mut output = output
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    writeln!(output, "{}", serde_json::to_string(message)?)?;
    output.flush()?;
    Ok(())
}
