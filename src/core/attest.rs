//! Attestations: a signer's endorsement of one recorded run.
//!
//! A signature covers a domain-separated digest of the run manifest. That is
//! enough to bind all of the evidence: the manifest names the run root, the
//! root folds every receipt id, every receipt id is the digest of its own body,
//! and every body names the digests of its output objects. Bundle import checks
//! that whole chain, so a valid attestation over the manifest vouches for every
//! byte behind it.
//!
//! A signature answers "who produced this". Whether that signer is believed is
//! the reader's decision, recorded in their own trust list; nothing here
//! decides it.

use std::collections::BTreeMap;
use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::core::digest;
use crate::core::receipt::RunManifest;

pub const KEY_PREFIX: &str = "ed25519:";

/// Separates flot run attestations from every other use of the same key.
const DOMAIN: &str = "flot-run-attestation-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    /// `ed25519:<hex>` public key of the signer.
    pub key: String,
    /// The manifest digest that was signed, so a mismatch can be reported as
    /// "the run changed" rather than as an opaque bad signature.
    pub manifest_digest: String,
    /// Hex Ed25519 signature over the domain-separated statement.
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttestError {
    MalformedKey(String),
    /// A small-order key can validate signatures over messages its holder never
    /// saw, so it proves nothing about authorship.
    WeakKey,
    MalformedSignature,
    /// The signature was made over a different run.
    ManifestChanged {
        signed: String,
        actual: String,
    },
    BadSignature,
}

impl fmt::Display for AttestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttestError::MalformedKey(reason) => write!(f, "malformed signer key: {reason}"),
            AttestError::WeakKey => write!(f, "signer key is a weak (small-order) key"),
            AttestError::MalformedSignature => write!(f, "malformed signature"),
            AttestError::ManifestChanged { signed, actual } => write!(
                f,
                "signed manifest {} but the run is {}",
                digest::short(signed),
                digest::short(actual)
            ),
            AttestError::BadSignature => write!(f, "signature does not verify"),
        }
    }
}

impl std::error::Error for AttestError {}

pub fn manifest_digest(manifest: &RunManifest) -> String {
    digest::canonical(manifest)
}

fn statement(manifest_digest: &str) -> Vec<u8> {
    format!("{DOMAIN}\n{manifest_digest}").into_bytes()
}

pub fn public_key_of(secret: &[u8; 32]) -> String {
    format_key(&SigningKey::from_bytes(secret).verifying_key())
}

fn format_key(key: &VerifyingKey) -> String {
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

/// How far a run's authorship is established for this reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authorship {
    /// No valid signature: integrity can be checked, origin cannot.
    Unsigned,
    /// Validly signed, but by no key this reader trusts.
    Untrusted,
    /// Validly signed by at least one key on this reader's trust list.
    Trusted,
}

impl Authorship {
    pub fn label(self) -> &'static str {
        match self {
            Authorship::Unsigned => "unsigned",
            Authorship::Untrusted => "untrusted",
            Authorship::Trusted => "trusted",
        }
    }
}

/// One attestation, judged against a run and a trust list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignerCheck {
    pub key: String,
    /// The name this reader trusts the key under, if any.
    pub trusted_as: Option<String>,
    /// Why the attestation does not hold, when it does not.
    pub problem: Option<String>,
}

impl SignerCheck {
    pub fn is_valid(&self) -> bool {
        self.problem.is_none()
    }
}

/// Judge every attestation. Only valid signatures count towards authorship;
/// an invalid one is reported, never ignored, because it means something was
/// altered after signing.
pub fn assess(
    attestations: &[Attestation],
    manifest: &RunManifest,
    trusted: &BTreeMap<String, String>,
) -> (Vec<SignerCheck>, Authorship) {
    let checks: Vec<SignerCheck> = attestations
        .iter()
        .map(|attestation| {
            let problem = check(attestation, manifest)
                .err()
                .map(|error| error.to_string());
            SignerCheck {
                trusted_as: problem
                    .is_none()
                    .then(|| trusted.get(&attestation.key).cloned())
                    .flatten(),
                key: attestation.key.clone(),
                problem,
            }
        })
        .collect();
    let authorship = if checks.iter().any(|check| check.trusted_as.is_some()) {
        Authorship::Trusted
    } else if checks.iter().any(SignerCheck::is_valid) {
        Authorship::Untrusted
    } else {
        Authorship::Unsigned
    };
    (checks, authorship)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::metrics::Spend;
    use crate::core::receipt::{ClaimCounts, Verdict};

    const SECRET: [u8; 32] = [7; 32];
    const OTHER: [u8; 32] = [9; 32];

    fn manifest(root: &str) -> RunManifest {
        RunManifest {
            run_id: "0000000000001-abc".into(),
            flow: "demo".into(),
            flow_digest: "sha256:flow".into(),
            root_digest: root.into(),
            verdict: Verdict::Passed,
            steps: Vec::new(),
            claims: ClaimCounts::default(),
            limitations: Vec::new(),
            max_parallel: 1,
            spend: Spend::default(),
            max_cost_micro_usd: None,
            started_at_ms: 0,
            duration_ms: 0,
            flot_version: "test".into(),
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
    fn authorship_counts_only_valid_signatures_and_only_trusted_keys() {
        let run = manifest("sha256:root");
        let mine = sign(&SECRET, &run);
        let theirs = sign(&OTHER, &run);
        let trust = BTreeMap::from([(mine.key.clone(), "alice".to_string())]);

        assert_eq!(assess(&[], &run, &trust).1, Authorship::Unsigned);
        assert_eq!(
            assess(std::slice::from_ref(&theirs), &run, &trust).1,
            Authorship::Untrusted
        );
        let (checks, authorship) = assess(&[theirs, mine.clone()], &run, &trust);
        assert_eq!(authorship, Authorship::Trusted);
        assert_eq!(checks[1].trusted_as.as_deref(), Some("alice"));

        // A trusted key whose signature no longer holds establishes nothing.
        let mut broken = mine;
        broken.signature = sign(&SECRET, &manifest("sha256:else")).signature;
        let (checks, authorship) = assess(&[broken], &run, &trust);
        assert_eq!(authorship, Authorship::Unsigned);
        assert!(!checks[0].is_valid());
        assert_eq!(checks[0].trusted_as, None);
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
}
