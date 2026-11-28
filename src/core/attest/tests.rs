//! Tests of signing a run, checking a signature and spelling keys, with the
//! helpers the attestation tests share.

use super::*;
use crate::core::metrics::Spend;
use crate::core::receipt::{ClaimCounts, RunManifest, Verdict};

mod authorship;
mod golden;
mod handover;
mod vouching;

const SECRET: [u8; 32] = [7; 32];
const OTHER: [u8; 32] = [9; 32];

fn manifest(root: &str) -> RunManifest {
    RunManifest {
        run_id: "0000000000001-abc".into(),
        flow: "demo".into(),
        flow_digest: "sha256:flow".into(),
        root_digest: root.into(),
        verdict: Verdict::Passed,
        partial: None,
        steps: Vec::new(),
        claims: ClaimCounts::default(),
        limitations: Vec::new(),
        max_parallel: 1,
        spend: Spend::default(),
        max_cost_micro_usd: None,
        started_at_ms: 0,
        duration_ms: 0,
        flot_version: "test".into(),
        refusals: Vec::new(),
    }
}

#[test]
fn a_fresh_attestation_checks_out() {
    let run = manifest("sha256:root");
    let attestation = sign(&SECRET, &run);
    assert_eq!(attestation.key, public_key_of(&SECRET));
    assert_eq!(check(&attestation, &run), Ok(()));
}

#[test]
fn signing_is_deterministic() {
    let run = manifest("sha256:root");
    assert_eq!(sign(&SECRET, &run), sign(&SECRET, &run));
}

#[test]
fn an_attestation_does_not_transfer_to_another_run() {
    let attestation = sign(&SECRET, &manifest("sha256:root"));
    assert!(matches!(
        check(&attestation, &manifest("sha256:other")),
        Err(AttestError::ManifestChanged { .. })
    ));
}

#[test]
fn retargeting_the_signed_digest_breaks_the_signature() {
    let other = manifest("sha256:other");
    let mut attestation = sign(&SECRET, &manifest("sha256:root"));
    attestation.manifest_digest = manifest_digest(&other);
    assert_eq!(check(&attestation, &other), Err(AttestError::BadSignature));
}

#[test]
fn swapping_in_another_key_breaks_the_signature() {
    let run = manifest("sha256:root");
    let mut attestation = sign(&SECRET, &run);
    attestation.key = public_key_of(&OTHER);
    assert_eq!(check(&attestation, &run), Err(AttestError::BadSignature));
}

#[test]
fn a_flipped_signature_bit_is_refused() {
    let run = manifest("sha256:root");
    let mut attestation = sign(&SECRET, &run);
    let mut bytes = hex::decode(&attestation.signature).unwrap();
    bytes[0] ^= 1;
    attestation.signature = hex::encode(bytes);
    assert_eq!(check(&attestation, &run), Err(AttestError::BadSignature));
}

#[test]
fn a_weak_key_is_refused_before_any_signature_check() {
    // The identity point: small order, so it "verifies" forged signatures.
    let mut identity = [0u8; 32];
    identity[0] = 1;
    let weak = format!("{KEY_PREFIX}{}", hex::encode(identity));
    assert_eq!(parse_key(&weak), Err(AttestError::WeakKey));
}

#[test]
fn malformed_keys_and_signatures_are_named() {
    assert!(matches!(
        parse_key("rsa:abcd"),
        Err(AttestError::MalformedKey(_))
    ));
    assert!(matches!(
        parse_key(&format!("{KEY_PREFIX}zz")),
        Err(AttestError::MalformedKey(_))
    ));
    let run = manifest("sha256:root");
    let mut attestation = sign(&SECRET, &run);
    attestation.signature = "00".into();
    assert_eq!(
        check(&attestation, &run),
        Err(AttestError::MalformedSignature)
    );
}

#[test]
fn canonical_key_normalizes_case_and_whitespace() {
    let key = public_key_of(&SECRET);
    let shouted = format!(
        "  {}{}  ",
        KEY_PREFIX,
        key[KEY_PREFIX.len()..].to_uppercase()
    );
    assert_eq!(canonical_key(&shouted).unwrap(), key);
}

#[test]
fn a_fingerprint_of_any_text_never_panics() {
    let hostile = format!("{KEY_PREFIX}{}é", "a".repeat(15));
    assert_eq!(
        fingerprint(&hostile),
        format!("{KEY_PREFIX}{}é", "a".repeat(15))
    );
    assert_eq!(fingerprint("é"), format!("{KEY_PREFIX}é"));
}
