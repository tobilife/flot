//! The protocol: initialization, notifications, malformed lines, and a
//! call cancelled while the server goes on answering.

use std::io::Write;
use std::sync::mpsc;

use serde_json::{json, Value};

use super::super::{Server, SUPPORTED_PROTOCOLS};
use super::{call, project, request, PROVEN};

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
fn an_unknown_method_is_a_jsonrpc_error() {
    let (_dir, server) = project(PROVEN);
    let response = server.handle(&request("tools/nope", json!({}))).unwrap();
    assert_eq!(response["error"]["code"], -32601);
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

#[test]
fn an_id_that_is_neither_a_string_nor_a_number_is_not_echoed() {
    let (_dir, server) = project(PROVEN);
    for id in [json!(true), json!({ "n": 1 }), json!([1])] {
        let reply = server.handle(&json!({ "jsonrpc": "2.0", "id": id }));
        assert_eq!(
            reply,
            Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32600, "message": "a request needs a string `method`" },
            })),
            "{id}"
        );
    }
}
