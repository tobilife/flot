//! Reading what a run kept: a step's output and its proof's, a receipt by
//! prefix, a trimmed object, and search.

use serde_json::json;

use super::{call, project, PROVEN};

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
    let mutants = call(
        &server,
        "flot_step_output",
        json!({ "step": "write", "mutants": true }),
    );
    assert_eq!(mutants["isError"], true, "{mutants}");
    assert!(
        mutants.to_string().contains("tested no mutants"),
        "{mutants}"
    );
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

    let receipt =
        call(&server, "flot_receipt", json!({ "receipt_id": prefix }))["structuredContent"].clone();
    assert_eq!(receipt["receipt_id"], receipt_id);

    let digest = receipt["identity"]["stdout_digest"].as_str().unwrap();
    let object =
        call(&server, "flot_read_object", json!({ "digest": digest }))["structuredContent"].clone();
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
