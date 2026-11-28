//! Tests of step keys and the facts they digest, and the helpers the other
//! keying tests share.

use super::*;
use crate::core::flow::Flow;

pub(super) fn step(extra: &str) -> Step {
    let text = format!("[flow]\nname = \"k\"\n\n[[step]]\nid = \"s\"\nrun = \"echo s\"\n{extra}\n\n[[step]]\nid = \"a\"\nrun = \"true\"\n\n[[step]]\nid = \"b\"\nrun = \"true\"\n");
    Flow::parse(&text).unwrap().step("s").unwrap().clone()
}

fn reads(inputs: &[FileDigest], verify_inputs: &[FileDigest]) -> Facts {
    Facts {
        inputs: inputs.to_vec(),
        verify_inputs: verify_inputs.to_vec(),
        limitations: Vec::new(),
    }
}

pub(super) fn input(path: &str, content: &[u8]) -> FileDigest {
    FileDigest {
        executable: false,
        path: path.into(),
        digest: digest::bytes(content),
    }
}

#[test]
fn the_key_tracks_input_content() {
    let step = step("");
    assert_ne!(
        step_key("k", &step, &reads(&[input("x", b"1")], &[]), &[]),
        step_key("k", &step, &reads(&[input("x", b"2")], &[]), &[])
    );
}

#[test]
fn the_key_tracks_the_proof_command() {
    assert_ne!(
        step_key("k", &step("verify = \"true\""), &Facts::default(), &[]),
        step_key("k", &step("verify = \"false\""), &Facts::default(), &[])
    );
}

#[test]
fn the_key_tracks_upstream_receipts() {
    let step = step("");
    let dep = |receipt: &str| DepRef {
        step_id: "a".into(),
        receipt_id: receipt.into(),
    };
    assert_ne!(
        step_key("k", &step, &Facts::default(), &[dep("sha256:1")]),
        step_key("k", &step, &Facts::default(), &[dep("sha256:2")])
    );
}

#[test]
fn dependency_references_ignore_declaration_order() {
    let receipts = HashMap::from([
        ("a".to_string(), "sha256:a".to_string()),
        ("b".to_string(), "sha256:b".to_string()),
    ]);
    let forward = dep_refs(&step("needs = [\"a\", \"b\"]"), &receipts).unwrap();
    let backward = dep_refs(&step("needs = [\"b\", \"a\"]"), &receipts).unwrap();
    assert_eq!(forward, backward);
}

#[test]
fn an_unsettled_dependency_makes_the_key_unknowable() {
    let receipts = HashMap::from([("a".to_string(), "sha256:a".to_string())]);
    assert!(dep_refs(&step("needs = [\"a\", \"b\"]"), &receipts).is_none());
}

#[test]
fn inputs_are_sorted_and_unmatched_patterns_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("b.txt"), "b").unwrap();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    let facts = facts_of(
        dir.path(),
        &step("inputs = [\"*.txt\", \"missing/**\"]"),
        &Digests::default(),
    );
    let (inputs, limitations) = (facts.inputs, facts.limitations);
    let paths: Vec<&str> = inputs.iter().map(|input| input.path.as_str()).collect();
    assert_eq!(paths, vec!["a.txt", "b.txt"]);
    assert_eq!(limitations.len(), 1);
    assert!(limitations[0].contains("missing/**"), "{}", limitations[0]);
}

#[test]
fn the_key_tracks_the_content_of_the_proof() {
    let step = step("verify = \"python3 check.py\"");
    assert_ne!(
        step_key("k", &step, &reads(&[], &[input("check.py", b"v1")]), &[]),
        step_key("k", &step, &reads(&[], &[input("check.py", b"v2")]), &[])
    );
}

#[test]
fn proof_files_and_work_files_are_not_interchangeable() {
    let step = step("verify = \"true\"");
    let file = input("x", b"1");
    assert_ne!(
        step_key("k", &step, &reads(std::slice::from_ref(&file), &[]), &[]),
        step_key("k", &step, &reads(&[], std::slice::from_ref(&file)), &[])
    );
}

/// The exact bytes a step key digests. Every receipt id, cache entry and
/// run root follows from them, so any change here must bump
/// `SCHEMA_VERSION`. Only flot's own version is filled in, so a release
/// leaves this test as it is.
#[test]
fn the_key_digests_exactly_these_bytes() {
    let golden = |text: &str| {
        digest::bytes(
            text.replace("@VERSION@", env!("CARGO_PKG_VERSION"))
                .as_bytes(),
        )
    };
    let facts = Facts {
        inputs: vec![
            FileDigest {
                executable: true,
                path: "run.sh".into(),
                digest: digest::bytes(b"make\n"),
            },
            input("src/a.txt", b"a"),
        ],
        verify_inputs: vec![input("check.sh", b"grep -q 1 out.txt\n")],
        limitations: vec!["not part of the key".into()],
    };
    let deps = [DepRef {
        step_id: "a".into(),
        receipt_id: digest::bytes(b"receipt of a"),
    }];

    let plain = step("");
    assert_eq!(
        step_key("k", &plain, &Facts::default(), &[]),
        golden(PLAIN_KEY),
        "a step that sets nothing optional"
    );

    let full = step(
        "verify = \"sh check.sh\"\nclaims = [\"out.txt holds 1\"]\nenv = { LANG = \"C\", A = \"1\" }\nallow_failure = true\noutputs = [\"out.txt\"]\ninputs = [\"src/*.txt\", \"run.sh\"]\nneeds = [\"a\"]\nmay_change = [\"notes/*\"]\nsandbox = { network = true, write = [\"out\"] }\nfails_before = true\nheld = [\"tests/*\"]\nmutants = { run = \"m\", report = \"r.json\", allow = \"allow.toml\" }\nattempts = 3\nbudget = { timeout_secs = 5 }",
    );
    assert_eq!(
        step_key("k", &full, &facts, &deps),
        golden(FULL_KEY),
        "a step that sets every field the key reads"
    );
}

const PLAIN_KEY: &str = r#"{"allow_failure":false,"claims":[],"command":"echo s","deps":[],"env":{},"flow":"k","inputs":[],"outputs":[],"schema":5,"step_id":"s","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify":null,"verify_inputs":[]}"#;

const FULL_KEY: &str = r#"{"allow_failure":true,"claims":["out.txt holds 1"],"command":"echo s","deps":[{"receipt_id":"sha256:0eba9c324da1c0ec8fc47af90ba25ab329eeb9744993eac814bdd8da10002cee","step_id":"a"}],"env":{"A":"1","LANG":"C"},"fails_before":true,"flow":"k","held":["tests/*"],"inputs":[{"digest":"sha256:c117ade9ea8a00abe926dfed79297a9a3632defb97a3ffd1baf33fe92704aa0f","executable":true,"path":"run.sh"},{"digest":"sha256:ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb","path":"src/a.txt"}],"may_change":["notes/*"],"mutants":{"allow":"allow.toml","report":"r.json","run":"m"},"outputs":["out.txt"],"sandbox":{"network":true,"write":["out"]},"schema":5,"step_id":"s","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify":"sh check.sh","verify_inputs":[{"digest":"sha256:a8b1f082c2590c4c3855b71427328ecab1ebb40701108a07be7b70f9c926f225","path":"check.sh"}]}"#;
