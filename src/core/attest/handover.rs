//! Key handovers: a statement, signed by both keys, that one key succeeds
//! another.

use ed25519_dalek::{Signature, Signer, SigningKey};
use serde::{Deserialize, Serialize};

use super::signing::format_key;
use super::{parse_key, AttestError};

/// Separates key handovers from run attestations made with the same keys,
/// and names what a handover file is.
pub const HANDOVER_KIND: &str = "flot-key-handover-v1";

/// A signer's statement that key `to` succeeds key `from`. Both keys sign
/// it: the old one vouches for its successor, and the new one shows that its
/// holder took the identity on, so nobody can hand a name to a key they do
/// not hold. It says nothing about runs; each reader decides whether to
/// follow it (`flot trust rotate`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handover {
    /// Always [`HANDOVER_KIND`].
    pub kind: String,
    pub from: String,
    pub to: String,
    pub from_signature: String,
    pub to_signature: String,
}

fn handover_statement(from: &str, to: &str) -> Vec<u8> {
    format!("{HANDOVER_KIND}\n{from}\n{to}").into_bytes()
}

pub fn hand_over(old: &[u8; 32], new: &[u8; 32]) -> Handover {
    let (old, new) = (SigningKey::from_bytes(old), SigningKey::from_bytes(new));
    let from = format_key(&old.verifying_key());
    let to = format_key(&new.verifying_key());
    let statement = handover_statement(&from, &to);
    Handover {
        kind: HANDOVER_KIND.to_string(),
        from_signature: hex::encode(old.sign(&statement).to_bytes()),
        to_signature: hex::encode(new.sign(&statement).to_bytes()),
        from,
        to,
    }
}

/// Check both signatures of a handover. Returns the canonical `(from, to)`.
pub fn check_handover(handover: &Handover) -> Result<(String, String), AttestError> {
    if handover.kind != HANDOVER_KIND {
        return Err(AttestError::NotAHandover);
    }
    let from = parse_key(handover.from.trim())?;
    let to = parse_key(handover.to.trim())?;
    if from == to {
        return Err(AttestError::SelfHandover);
    }
    let (from_text, to_text) = (format_key(&from), format_key(&to));
    let statement = handover_statement(&from_text, &to_text);
    for (key, signature) in [
        (&from, &handover.from_signature),
        (&to, &handover.to_signature),
    ] {
        let bytes: [u8; 64] = hex::decode(signature)
            .map_err(|_| AttestError::MalformedSignature)?
            .try_into()
            .map_err(|_| AttestError::MalformedSignature)?;
        key.verify_strict(&statement, &Signature::from_bytes(&bytes))
            .map_err(|_| AttestError::BadSignature)?;
    }
    Ok((from_text, to_text))
}
