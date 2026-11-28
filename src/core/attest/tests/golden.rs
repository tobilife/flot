//! The exact bytes a run manifest is stored as, digested as and signed over.

use crate::core::digest;
use crate::core::metrics::Spend;
use crate::core::receipt::{ClaimCounts, RunManifest, RunStep, StepStatus, Verdict};

use super::super::{manifest_digest, public_key_of, sign};
use super::{manifest, OTHER, SECRET};

/// A manifest with every optional part set, named after a release that
/// never happens so that releases leave the goldens below as they are.
fn golden_manifest() -> RunManifest {
    use crate::core::receipt::{Partial, Refusal, RunAttempt, RunCandidate};
    let root_digest = digest::bytes(b"root");
    RunManifest {
        run_id: RunManifest::id_for(1_700_000_000_000, &root_digest),
        flow: "golden".into(),
        flow_digest: digest::bytes(b"flow"),
        root_digest,
        verdict: Verdict::Unverified,
        partial: Some(Partial { selected: 1, of: 2 }),
        steps: vec![RunStep {
            step_id: "build".into(),
            receipt_id: digest::bytes(b"receipt"),
            status: StepStatus::Ok,
            cached: true,
            claims: ClaimCounts {
                verified: 1,
                unverified: 1,
                refuted: 0,
            },
            observed_digest: digest::bytes(b"observed"),
            attempts: vec![RunAttempt {
                receipt_id: digest::bytes(b"first attempt"),
                observed_digest: digest::bytes(b"first observed"),
            }],
            candidates: vec![RunCandidate {
                name: "slow".into(),
                receipt_id: digest::bytes(b"slow receipt"),
                observed_digest: digest::bytes(b"slow observed"),
                attempts: Vec::new(),
            }],
            vouched_by: vec![public_key_of(&OTHER)],
        }],
        claims: ClaimCounts {
            verified: 1,
            unverified: 1,
            refuted: 0,
        },
        limitations: vec!["build: step declares no inputs".into()],
        max_parallel: 2,
        spend: Spend {
            input_tokens: 10,
            output_tokens: 20,
            cost_micro_usd: 300,
            reporting_steps: 1,
            silent_steps: 0,
            unpriced_steps: 0,
            reused_cost_micro_usd: 50,
        },
        max_cost_micro_usd: Some(1_000_000),
        started_at_ms: 1_700_000_000_000,
        duration_ms: 1234,
        flot_version: "0.0.0-golden".into(),
        refusals: vec![Refusal {
            at_ms: 1_699_999_999_000,
            tool: "Bash".into(),
            reasons: vec!["check.sh: the proof of step `build` reads it".into()],
        }],
    }
}

/// The exact bytes a run is stored as and signed over. Field order is
/// byte order, so these change only with the store format.
#[test]
fn a_run_manifest_has_exactly_these_bytes() {
    let run = golden_manifest();
    assert_eq!(
        serde_json::to_string_pretty(&run).unwrap(),
        GOLDEN_MANIFEST,
        "the text the store keeps"
    );
    assert_eq!(
        digest::canonical_json(&run),
        GOLDEN_MANIFEST_CANONICAL,
        "the body the manifest digest covers"
    );
    assert_eq!(manifest_digest(&run), GOLDEN_MANIFEST_DIGEST);
    let attestation = sign(&SECRET, &run);
    assert_eq!(attestation.key, GOLDEN_KEY);
    assert_eq!(attestation.signature, GOLDEN_SIGNATURE);

    // Unset optional parts leave no trace in what is signed.
    assert_eq!(
        manifest_digest(&manifest("sha256:root")),
        GOLDEN_PLAIN_MANIFEST_DIGEST
    );
}

const GOLDEN_MANIFEST: &str = r#"{
  "run_id": "1700000000000-4813494d137e",
  "flow": "golden",
  "flow_digest": "sha256:3b21278178ed3463f3bde3a35f088caf6d99ff1b2f647d9a0c2b8de599ec3dc6",
  "root_digest": "sha256:4813494d137e1631bba301d5acab6e7bb7aa74ce1185d456565ef51d737677b2",
  "verdict": "unverified",
  "partial": {
    "selected": 1,
    "of": 2
  },
  "steps": [
    {
      "step_id": "build",
      "receipt_id": "sha256:6f32860910ca0fb2a20c7fda143666b09dbf8db5238195c90a586fb542ff0cad",
      "status": "ok",
      "cached": true,
      "claims": {
        "verified": 1,
        "unverified": 1,
        "refuted": 0
      },
      "observed_digest": "sha256:604cee807f644af47487bf2bbab442b94212ac5119f36f995f78e9e4694dae8c",
      "attempts": [
        {
          "receipt_id": "sha256:e28dc1711113db3c923d5fe79362c8ded50fc4056e06742807f882d75d095a43",
          "observed_digest": "sha256:301af7b83013734737a02b5104a9b52d591fb3483d63c8d819b65721301765cc"
        }
      ],
      "candidates": [
        {
          "name": "slow",
          "receipt_id": "sha256:ab9cc7ee5dde44bcc6defc4a5859044b58dc974d54eb54fe606e86a663b88bec",
          "observed_digest": "sha256:57f850e54eacf02801f0a56aba1e9accddfd3877b2a12e4f15a3d31ee1cff0b4"
        }
      ],
      "vouched_by": [
        "ed25519:fd1724385aa0c75b64fb78cd602fa1d991fdebf76b13c58ed702eac835e9f618"
      ]
    }
  ],
  "claims": {
    "verified": 1,
    "unverified": 1,
    "refuted": 0
  },
  "limitations": [
    "build: step declares no inputs"
  ],
  "max_parallel": 2,
  "spend": {
    "input_tokens": 10,
    "output_tokens": 20,
    "cost_micro_usd": 300,
    "reporting_steps": 1,
    "silent_steps": 0,
    "unpriced_steps": 0,
    "reused_cost_micro_usd": 50
  },
  "max_cost_micro_usd": 1000000,
  "started_at_ms": 1700000000000,
  "duration_ms": 1234,
  "flot_version": "0.0.0-golden",
  "refusals": [
    {
      "at_ms": 1699999999000,
      "tool": "Bash",
      "reasons": [
        "check.sh: the proof of step `build` reads it"
      ]
    }
  ]
}"#;

const GOLDEN_MANIFEST_CANONICAL: &str = r#"{"claims":{"refuted":0,"unverified":1,"verified":1},"duration_ms":1234,"flot_version":"0.0.0-golden","flow":"golden","flow_digest":"sha256:3b21278178ed3463f3bde3a35f088caf6d99ff1b2f647d9a0c2b8de599ec3dc6","limitations":["build: step declares no inputs"],"max_cost_micro_usd":1000000,"max_parallel":2,"partial":{"of":2,"selected":1},"refusals":[{"at_ms":1699999999000,"reasons":["check.sh: the proof of step `build` reads it"],"tool":"Bash"}],"root_digest":"sha256:4813494d137e1631bba301d5acab6e7bb7aa74ce1185d456565ef51d737677b2","run_id":"1700000000000-4813494d137e","spend":{"cost_micro_usd":300,"input_tokens":10,"output_tokens":20,"reporting_steps":1,"reused_cost_micro_usd":50,"silent_steps":0,"unpriced_steps":0},"started_at_ms":1700000000000,"steps":[{"attempts":[{"observed_digest":"sha256:301af7b83013734737a02b5104a9b52d591fb3483d63c8d819b65721301765cc","receipt_id":"sha256:e28dc1711113db3c923d5fe79362c8ded50fc4056e06742807f882d75d095a43"}],"cached":true,"candidates":[{"name":"slow","observed_digest":"sha256:57f850e54eacf02801f0a56aba1e9accddfd3877b2a12e4f15a3d31ee1cff0b4","receipt_id":"sha256:ab9cc7ee5dde44bcc6defc4a5859044b58dc974d54eb54fe606e86a663b88bec"}],"claims":{"refuted":0,"unverified":1,"verified":1},"observed_digest":"sha256:604cee807f644af47487bf2bbab442b94212ac5119f36f995f78e9e4694dae8c","receipt_id":"sha256:6f32860910ca0fb2a20c7fda143666b09dbf8db5238195c90a586fb542ff0cad","status":"ok","step_id":"build","vouched_by":["ed25519:fd1724385aa0c75b64fb78cd602fa1d991fdebf76b13c58ed702eac835e9f618"]}],"verdict":"unverified"}"#;

const GOLDEN_MANIFEST_DIGEST: &str =
    r#"sha256:48317c591287ad975589a79cb6048ac3224355251a4ff672b676a1cb603eb91e"#;

const GOLDEN_KEY: &str =
    r#"ed25519:ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"#;

const GOLDEN_SIGNATURE: &str = r#"fd5696bb132b6f369cd62019a92efee2ace8b6dd072a2f0983da3d15fd80b4a6efaee4086c36f2b4b0b0783de02169b03639d2e2c2f3b227c7b9c8dbb0a2e80a"#;

const GOLDEN_PLAIN_MANIFEST_DIGEST: &str =
    r#"sha256:09ad2f7777cb7425ccff44b865c4362aa9677262a2cf35e04ff047742d9806d0"#;
