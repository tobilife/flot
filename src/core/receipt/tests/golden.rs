//! The exact bytes of a receipt: the body its id digests, the text the store
//! keeps and the observation a run stores.

use std::collections::BTreeMap;

use crate::core::digest;
use crate::core::flow::SCHEMA_VERSION;
use crate::core::metrics::Metrics;
use crate::core::receipt::{
    Before, ClaimRecord, ClaimStatus, DepRef, FileDigest, Observed, Receipt, ReceiptIdentity,
    SandboxRecord, StepStatus, ToolIdentity,
};

/// A receipt with every optional part set, named after a release that
/// never happens so that releases leave the goldens below as they are.
fn golden_receipt() -> Receipt {
    let identity = ReceiptIdentity {
        flow: "golden".into(),
        step_id: "build".into(),
        step_key: digest::bytes(b"step key"),
        command: "make out.txt".into(),
        status: StepStatus::Ok,
        exit_code: Some(0),
        stdout_digest: digest::bytes(b"out\n"),
        stderr_digest: digest::bytes(b""),
        inputs: vec![
            FileDigest {
                path: "run.sh".into(),
                digest: digest::bytes(b"make\n"),
                executable: true,
            },
            FileDigest {
                path: "src/a.txt".into(),
                digest: digest::bytes(b"a"),
                executable: false,
            },
        ],
        verify_inputs: vec![FileDigest {
            path: "check.sh".into(),
            digest: digest::bytes(b"grep -q 1 out.txt\n"),
            executable: false,
        }],
        outputs: vec![FileDigest {
            path: "out.txt".into(),
            digest: digest::bytes(b"1\n"),
            executable: false,
        }],
        changed: BTreeMap::from([
            ("notes.md".into(), Some(digest::bytes(b"notes\n"))),
            ("old.txt".into(), None),
        ]),
        deps: vec![DepRef {
            step_id: "fetch".into(),
            receipt_id: digest::bytes(b"receipt of fetch"),
        }],
        claims: vec![
            ClaimRecord {
                text: "out.txt holds 1".into(),
                status: ClaimStatus::Verified,
                verify_command: Some("sh check.sh".into()),
                verify_exit: Some(0),
                verify_output_digest: Some(digest::bytes(b"")),
                reason: None,
                before: Some(Before {
                    commit: "0123456789abcdef0123456789abcdef01234567".into(),
                    exit_code: Some(1),
                    output_digest: digest::bytes(b"no\n"),
                }),
                held: None,
                mutants: None,
                differed: None,
            },
            ClaimRecord {
                text: "the docs say so".into(),
                status: ClaimStatus::Unverified,
                verify_command: None,
                verify_exit: None,
                verify_output_digest: None,
                reason: Some("no verify command".into()),
                before: None,
                held: None,
                mutants: None,
                differed: None,
            },
        ],
        limitations: vec!["step declares no inputs".into()],
        tool: ToolIdentity {
            name: "flot".into(),
            version: "0.0.0-golden".into(),
            schema: SCHEMA_VERSION,
        },
        candidate: Some("fast".into()),
        sandbox: Some(SandboxRecord {
            network: false,
            write: Some(vec!["out".into()]),
            mechanism: "landlock abi 7, network namespace".into(),
        }),
    };
    let observed = Observed {
        started_at_ms: 1_700_000_000_000,
        duration_ms: 1234,
        host_os: "linux".into(),
        stdout_inline: "out\n".into(),
        stdout_truncated: false,
        stderr_inline: String::new(),
        stderr_truncated: false,
        stdout_bytes: 4,
        stderr_bytes: 0,
        metrics: Some(Metrics {
            input_tokens: 10,
            output_tokens: 20,
            cost_micro_usd: Some(300),
        }),
        metrics_notes: vec!["cost was rounded".into()],
    };
    Receipt::seal(identity, observed)
}

/// The exact bytes a receipt is named by, stored as and observed as.
/// Field order is byte order, so these change only with `SCHEMA_VERSION`
/// or the store format.
#[test]
fn a_receipt_has_exactly_these_bytes() {
    let receipt = golden_receipt();
    assert_eq!(
        digest::canonical_json(&receipt.identity),
        GOLDEN_IDENTITY,
        "the body the receipt id digests"
    );
    assert_eq!(receipt.receipt_id, GOLDEN_RECEIPT_ID);
    assert_eq!(
        serde_json::to_string_pretty(&receipt).unwrap(),
        GOLDEN_RECEIPT,
        "the text the store keeps"
    );
    assert_eq!(
        digest::canonical_json(&receipt.observed),
        GOLDEN_OBSERVED,
        "the observation a run stores"
    );
    assert_eq!(receipt.observed_digest(), GOLDEN_OBSERVED_DIGEST);

    // Unset optional parts leave no trace in the body.
    let mut quiet = receipt.identity.clone();
    quiet.changed.clear();
    quiet.claims.clear();
    quiet.inputs.clear();
    quiet.candidate = None;
    quiet.sandbox = None;
    let quiet = Receipt::seal(quiet, Observed::default());
    assert_eq!(
        digest::canonical_json(&quiet.identity),
        GOLDEN_QUIET_IDENTITY
    );
    assert_eq!(quiet.receipt_id, GOLDEN_QUIET_RECEIPT_ID);
}

const GOLDEN_IDENTITY: &str = r#"{"candidate":"fast","changed":{"notes.md":"sha256:444e0fffbd825e9610ff5b199485707a0c895339ae80c15cc8a8aee41b106fda","old.txt":null},"claims":[{"before":{"commit":"0123456789abcdef0123456789abcdef01234567","exit_code":1,"output_digest":"sha256:564739ea8fa5926d4fa5c9734fed462061960a22e6b8d5c06e94969d97891bf2"},"reason":null,"status":"verified","text":"out.txt holds 1","verify_command":"sh check.sh","verify_exit":0,"verify_output_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"},{"reason":"no verify command","status":"unverified","text":"the docs say so","verify_command":null,"verify_exit":null,"verify_output_digest":null}],"command":"make out.txt","deps":[{"receipt_id":"sha256:ddb195b83f23632aecf220803ccfb3b94b79146dbca28b2a7014df8e86766861","step_id":"fetch"}],"exit_code":0,"flow":"golden","inputs":[{"digest":"sha256:c117ade9ea8a00abe926dfed79297a9a3632defb97a3ffd1baf33fe92704aa0f","executable":true,"path":"run.sh"},{"digest":"sha256:ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb","path":"src/a.txt"}],"limitations":["step declares no inputs"],"outputs":[{"digest":"sha256:4355a46b19d348dc2f57c046f8ef63d4538ebb936000f3c9ee954a27460dd865","path":"out.txt"}],"sandbox":{"mechanism":"landlock abi 7, network namespace","network":false,"write":["out"]},"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:54034ac5c6e9ea95734ec2b729fd6d62abf64af34a9f9ce5d466cb788191a73d","step_id":"build","step_key":"sha256:9bcae35dd64134b0c70ba26408a1b8f9f90d80123bdc5462d44d203de3d939f7","tool":{"name":"flot","schema":5,"version":"0.0.0-golden"},"verify_inputs":[{"digest":"sha256:a8b1f082c2590c4c3855b71427328ecab1ebb40701108a07be7b70f9c926f225","path":"check.sh"}]}"#;

const GOLDEN_RECEIPT_ID: &str =
    r#"sha256:5c7388eef6d66bbab54cfa95dd33f80b29975e31a7c2a8f9ffcca475e1f9f232"#;

const GOLDEN_RECEIPT: &str = r#"{
  "receipt_id": "sha256:5c7388eef6d66bbab54cfa95dd33f80b29975e31a7c2a8f9ffcca475e1f9f232",
  "identity": {
    "flow": "golden",
    "step_id": "build",
    "step_key": "sha256:9bcae35dd64134b0c70ba26408a1b8f9f90d80123bdc5462d44d203de3d939f7",
    "command": "make out.txt",
    "status": "ok",
    "exit_code": 0,
    "stdout_digest": "sha256:54034ac5c6e9ea95734ec2b729fd6d62abf64af34a9f9ce5d466cb788191a73d",
    "stderr_digest": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "inputs": [
      {
        "path": "run.sh",
        "digest": "sha256:c117ade9ea8a00abe926dfed79297a9a3632defb97a3ffd1baf33fe92704aa0f",
        "executable": true
      },
      {
        "path": "src/a.txt",
        "digest": "sha256:ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"
      }
    ],
    "verify_inputs": [
      {
        "path": "check.sh",
        "digest": "sha256:a8b1f082c2590c4c3855b71427328ecab1ebb40701108a07be7b70f9c926f225"
      }
    ],
    "outputs": [
      {
        "path": "out.txt",
        "digest": "sha256:4355a46b19d348dc2f57c046f8ef63d4538ebb936000f3c9ee954a27460dd865"
      }
    ],
    "changed": {
      "notes.md": "sha256:444e0fffbd825e9610ff5b199485707a0c895339ae80c15cc8a8aee41b106fda",
      "old.txt": null
    },
    "deps": [
      {
        "step_id": "fetch",
        "receipt_id": "sha256:ddb195b83f23632aecf220803ccfb3b94b79146dbca28b2a7014df8e86766861"
      }
    ],
    "claims": [
      {
        "text": "out.txt holds 1",
        "status": "verified",
        "verify_command": "sh check.sh",
        "verify_exit": 0,
        "verify_output_digest": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "reason": null,
        "before": {
          "commit": "0123456789abcdef0123456789abcdef01234567",
          "exit_code": 1,
          "output_digest": "sha256:564739ea8fa5926d4fa5c9734fed462061960a22e6b8d5c06e94969d97891bf2"
        }
      },
      {
        "text": "the docs say so",
        "status": "unverified",
        "verify_command": null,
        "verify_exit": null,
        "verify_output_digest": null,
        "reason": "no verify command"
      }
    ],
    "limitations": [
      "step declares no inputs"
    ],
    "tool": {
      "name": "flot",
      "version": "0.0.0-golden",
      "schema": 5
    },
    "candidate": "fast",
    "sandbox": {
      "network": false,
      "write": [
        "out"
      ],
      "mechanism": "landlock abi 7, network namespace"
    }
  },
  "observed": {
    "started_at_ms": 1700000000000,
    "duration_ms": 1234,
    "host_os": "linux",
    "stdout_inline": "out\n",
    "stdout_truncated": false,
    "stderr_inline": "",
    "stderr_truncated": false,
    "stdout_bytes": 4,
    "stderr_bytes": 0,
    "metrics": {
      "input_tokens": 10,
      "output_tokens": 20,
      "cost_micro_usd": 300
    },
    "metrics_notes": [
      "cost was rounded"
    ]
  }
}"#;

const GOLDEN_OBSERVED: &str = r#"{"duration_ms":1234,"host_os":"linux","metrics":{"cost_micro_usd":300,"input_tokens":10,"output_tokens":20},"metrics_notes":["cost was rounded"],"started_at_ms":1700000000000,"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":4,"stdout_inline":"out\n","stdout_truncated":false}"#;

const GOLDEN_OBSERVED_DIGEST: &str =
    r#"sha256:afc80763eb9e085082778af793b5e4ceca56ce8e4cac9a82a243b2d110e2a73d"#;

const GOLDEN_QUIET_IDENTITY: &str = r#"{"claims":[],"command":"make out.txt","deps":[{"receipt_id":"sha256:ddb195b83f23632aecf220803ccfb3b94b79146dbca28b2a7014df8e86766861","step_id":"fetch"}],"exit_code":0,"flow":"golden","inputs":[],"limitations":["step declares no inputs"],"outputs":[{"digest":"sha256:4355a46b19d348dc2f57c046f8ef63d4538ebb936000f3c9ee954a27460dd865","path":"out.txt"}],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:54034ac5c6e9ea95734ec2b729fd6d62abf64af34a9f9ce5d466cb788191a73d","step_id":"build","step_key":"sha256:9bcae35dd64134b0c70ba26408a1b8f9f90d80123bdc5462d44d203de3d939f7","tool":{"name":"flot","schema":5,"version":"0.0.0-golden"},"verify_inputs":[{"digest":"sha256:a8b1f082c2590c4c3855b71427328ecab1ebb40701108a07be7b70f9c926f225","path":"check.sh"}]}"#;

const GOLDEN_QUIET_RECEIPT_ID: &str =
    r#"sha256:6bf62e8bf4a81c2fcf69813f3c010d1bfd66193a944438aa6bbdfa8f43a7e328"#;
