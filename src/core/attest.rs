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

mod authorship;
mod handover;
mod signing;
mod vouching;

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::core::digest;

pub use authorship::{assess, Authorship, SignerCheck};
pub use handover::{check_handover, hand_over, Handover, HANDOVER_KIND};
pub use signing::{
    canonical_key, check, fingerprint, manifest_digest, parse_key, public_key_of, sign, KEY_PREFIX,
};
pub use vouching::{vouched, Endorsements, VouchedStep, Voucher};

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
    /// The file is something other than a flot key handover.
    NotAHandover,
    /// A key cannot succeed itself.
    SelfHandover,
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
            AttestError::NotAHandover => write!(f, "not a {HANDOVER_KIND} statement"),
            AttestError::SelfHandover => write!(f, "the handover names the same key twice"),
        }
    }
}

impl std::error::Error for AttestError {}

#[cfg(test)]
mod tests;
