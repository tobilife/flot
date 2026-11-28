//! How far a run's authorship is established for a reader: every
//! attestation judged against the run and the reader's trust list.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::core::receipt::RunManifest;

use super::{check, Attestation};

/// How far a run's authorship is established for this reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authorship {
    /// No valid signature: integrity can be checked, origin cannot.
    Unsigned,
    /// Validly signed, but by no key this reader trusts.
    Untrusted,
    /// Validly signed by the key this reader holds, which is not on the
    /// trust list. A label, not trust.
    ThisHost,
    /// Validly signed by at least one key on this reader's trust list.
    Trusted,
}

impl Authorship {
    pub fn label(self) -> &'static str {
        match self {
            Authorship::Unsigned => "unsigned",
            Authorship::Untrusted => "untrusted",
            Authorship::ThisHost => "this host's key",
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
    /// The signature holds and is by the key this reader holds.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub this_host: bool,
}

impl SignerCheck {
    pub fn is_valid(&self) -> bool {
        self.problem.is_none()
    }
}

/// Judge every attestation. Only valid signatures count towards authorship;
/// an invalid one is reported, never ignored, because it means something was
/// altered after signing. `host_key` is the public key the reader holds the
/// secret of, if any: a valid signature by it is labeled, never trusted.
pub fn assess(
    attestations: &[Attestation],
    manifest: &RunManifest,
    trusted: &BTreeMap<String, String>,
    host_key: Option<&str>,
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
                this_host: problem.is_none() && host_key == Some(attestation.key.as_str()),
                key: attestation.key.clone(),
                problem,
            }
        })
        .collect();
    let authorship = if checks.iter().any(|check| check.trusted_as.is_some()) {
        Authorship::Trusted
    } else if checks.iter().any(|check| check.this_host) {
        Authorship::ThisHost
    } else if checks.iter().any(SignerCheck::is_valid) {
        Authorship::Untrusted
    } else {
        Authorship::Unsigned
    };
    (checks, authorship)
}
