//! Keys as flot spells them, signing a run, and checking a signature over
//! one.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::core::digest;
use crate::core::receipt::RunManifest;

use super::{AttestError, Attestation};

pub const KEY_PREFIX: &str = "ed25519:";

/// Separates flot run attestations from every other use of the same key.
const DOMAIN: &str = "flot-run-attestation-v1";

pub fn manifest_digest(manifest: &RunManifest) -> String {
    digest::canonical(manifest)
}

fn statement(manifest_digest: &str) -> Vec<u8> {
    format!("{DOMAIN}\n{manifest_digest}").into_bytes()
}

pub fn public_key_of(secret: &[u8; 32]) -> String {
    format_key(&SigningKey::from_bytes(secret).verifying_key())
}

pub(super) fn format_key(key: &VerifyingKey) -> String {
    format!("{KEY_PREFIX}{}", hex::encode(key.to_bytes()))
}

/// Parse an `ed25519:<hex>` key, refusing weak keys.
pub fn parse_key(text: &str) -> Result<VerifyingKey, AttestError> {
    let hex_part = text
        .strip_prefix(KEY_PREFIX)
        .ok_or_else(|| AttestError::MalformedKey(format!("expected `{KEY_PREFIX}<hex>`")))?;
    let bytes: [u8; 32] = hex::decode(hex_part)
        .map_err(|error| AttestError::MalformedKey(error.to_string()))?
        .try_into()
        .map_err(|_| AttestError::MalformedKey("expected 32 bytes".into()))?;
    let key = VerifyingKey::from_bytes(&bytes)
        .map_err(|_| AttestError::MalformedKey("not a curve point".into()))?;
    if key.is_weak() {
        return Err(AttestError::WeakKey);
    }
    Ok(key)
}

/// Normalize a user-supplied key to its canonical spelling.
pub fn canonical_key(text: &str) -> Result<String, AttestError> {
    parse_key(text.trim()).map(|key| format_key(&key))
}

pub fn sign(secret: &[u8; 32], manifest: &RunManifest) -> Attestation {
    let signing_key = SigningKey::from_bytes(secret);
    let manifest_digest = manifest_digest(manifest);
    let signature = signing_key.sign(&statement(&manifest_digest));
    Attestation {
        key: format_key(&signing_key.verifying_key()),
        manifest_digest,
        signature: hex::encode(signature.to_bytes()),
    }
}

/// Check an attestation against the run it claims to cover.
pub fn check(attestation: &Attestation, manifest: &RunManifest) -> Result<(), AttestError> {
    let key = parse_key(&attestation.key)?;
    let actual = manifest_digest(manifest);
    if attestation.manifest_digest != actual {
        return Err(AttestError::ManifestChanged {
            signed: attestation.manifest_digest.clone(),
            actual,
        });
    }
    let bytes: [u8; 64] = hex::decode(&attestation.signature)
        .map_err(|_| AttestError::MalformedSignature)?
        .try_into()
        .map_err(|_| AttestError::MalformedSignature)?;
    key.verify_strict(&statement(&actual), &Signature::from_bytes(&bytes))
        .map_err(|_| AttestError::BadSignature)
}

/// A short, stable label for a key, for display only.
pub fn fingerprint(key: &str) -> String {
    let hex_part = key.strip_prefix(KEY_PREFIX).unwrap_or(key);
    // Keys come from bundles, so this one may be anything: cut on characters.
    format!(
        "{KEY_PREFIX}{}",
        hex_part.chars().take(16).collect::<String>()
    )
}
