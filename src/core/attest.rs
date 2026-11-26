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

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::core::digest;
use crate::core::receipt::RunManifest;

pub const KEY_PREFIX: &str = "ed25519:";

/// Separates flot run attestations from every other use of the same key.
const DOMAIN: &str = "flot-run-attestation-v1";

/// Separates key handovers from run attestations made with the same keys,
/// and names what a handover file is.
pub const HANDOVER_KIND: &str = "flot-key-handover-v1";

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

/// A short, stable label for a key, for display only.
pub fn fingerprint(key: &str) -> String {
    let hex_part = key.strip_prefix(KEY_PREFIX).unwrap_or(key);
    // Keys come from bundles, so this one may be anything: cut on characters.
    format!(
        "{KEY_PREFIX}{}",
        hex_part.chars().take(16).collect::<String>()
    )
}

/// The receipts signers vouched for, as far as a reader can check: each step
/// of each run it holds, under every key whose attestation of that run holds.
///
/// A run records who vouched for a step it reused, but that record is only
/// the run's word. The run CI executes is unsigned, so nothing covers it, and
/// an imported run can name any key. Only a signature by the key itself over
/// a run that kept the same receipt for the step confirms it.
#[derive(Debug, Clone, Default)]
pub struct Endorsements {
    /// Signer key, step id, receipt id.
    signed: BTreeSet<(String, String, String)>,
}

impl Endorsements {
    /// Count the steps of `manifest` as endorsed by each key in
    /// `attestations` whose signature over it holds. An attestation that does
    /// not hold endorses nothing.
    pub fn add(&mut self, attestations: &[Attestation], manifest: &RunManifest) {
        for attestation in attestations {
            if check(attestation, manifest).is_err() {
                continue;
            }
            for step in &manifest.steps {
                self.signed.insert((
                    attestation.key.clone(),
                    step.step_id.clone(),
                    step.receipt_id.clone(),
                ));
            }
        }
    }

    /// Whether `key` signed a run that kept `receipt_id` for `step_id`.
    pub fn endorse(&self, key: &str, step_id: &str, receipt_id: &str) -> bool {
        self.signed
            .contains(&(key.to_string(), step_id.to_string(), receipt_id.to_string()))
    }
}

/// A signer a run names for a step it reused from an imported run, judged by
/// a reader. Whether to believe the signer is the reader's decision, and may
/// change, so it is judged when the run is read, never recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Voucher {
    pub key: String,
    /// Whether the reader holds a run this key signed that kept the same
    /// receipt for the step.
    pub confirmed: bool,
    /// The name the reader trusts the key under, when it is confirmed.
    pub trusted_as: Option<String>,
}

impl Voucher {
    /// How a report names it: by its trusted name only when it is confirmed
    /// and trusted, so neither a signer named without a signature nor a
    /// revoked one ever reads as one this reader believes.
    pub fn label(&self) -> String {
        match (self.confirmed, &self.trusted_as) {
            (false, _) => format!("{} (not confirmed here)", fingerprint(&self.key)),
            (true, None) => format!("{} (not trusted here)", fingerprint(&self.key)),
            (true, Some(name)) => name.clone(),
        }
    }
}

/// A step a run reused from an imported run, and who vouched for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VouchedStep {
    pub step_id: String,
    pub vouched_by: Vec<Voucher>,
}

/// The steps of `manifest` that name who vouched for them, in run order,
/// with each signer confirmed by `endorsements` and named by `trusted`.
pub fn vouched(
    manifest: &RunManifest,
    trusted: &BTreeMap<String, String>,
    endorsements: &Endorsements,
) -> Vec<VouchedStep> {
    manifest
        .steps
        .iter()
        .filter(|step| !step.vouched_by.is_empty())
        .map(|step| VouchedStep {
            step_id: step.step_id.clone(),
            vouched_by: step
                .vouched_by
                .iter()
                .map(|key| {
                    let confirmed = endorsements.endorse(key, &step.step_id, &step.receipt_id);
                    Voucher {
                        key: key.clone(),
                        confirmed,
                        trusted_as: if confirmed {
                            trusted.get(key).cloned()
                        } else {
                            None
                        },
                    }
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::metrics::Spend;
    use crate::core::receipt::{ClaimCounts, RunStep, StepStatus, Verdict};

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
    fn authorship_counts_only_valid_signatures_and_only_trusted_keys() {
        let run = manifest("sha256:root");
        let mine = sign(&SECRET, &run);
        let theirs = sign(&OTHER, &run);
        let trust = BTreeMap::from([(mine.key.clone(), "alice".to_string())]);

        assert_eq!(assess(&[], &run, &trust, None).1, Authorship::Unsigned);
        assert_eq!(
            assess(std::slice::from_ref(&theirs), &run, &trust, None).1,
            Authorship::Untrusted
        );
        let (checks, authorship) = assess(&[theirs, mine.clone()], &run, &trust, None);
        assert_eq!(authorship, Authorship::Trusted);
        assert_eq!(checks[1].trusted_as.as_deref(), Some("alice"));

        // A trusted key whose signature no longer holds establishes nothing.
        let mut broken = mine;
        broken.signature = sign(&SECRET, &manifest("sha256:else")).signature;
        let (checks, authorship) = assess(&[broken], &run, &trust, None);
        assert_eq!(authorship, Authorship::Unsigned);
        assert!(!checks[0].is_valid());
        assert_eq!(checks[0].trusted_as, None);
    }

    #[test]
    fn a_signature_by_this_hosts_key_is_labeled_and_never_trusted() {
        let run = manifest("sha256:root");
        let mine = sign(&SECRET, &run);
        let theirs = sign(&OTHER, &run);
        let host_key = mine.key.clone();
        let host = Some(host_key.as_str());
        let nobody = BTreeMap::new();

        let (checks, authorship) = assess(&[theirs.clone(), mine.clone()], &run, &nobody, host);
        assert_eq!(authorship, Authorship::ThisHost);
        assert!(!checks[0].this_host);
        assert!(checks[1].this_host);
        assert_eq!(checks[1].trusted_as, None);

        // Another host's key is no reason for the label.
        assert_eq!(
            assess(std::slice::from_ref(&theirs), &run, &nobody, host).1,
            Authorship::Untrusted
        );

        // The trust list still decides: a trusted signer outranks the label.
        let trust = BTreeMap::from([(theirs.key.clone(), "bob".to_string())]);
        assert_eq!(
            assess(&[theirs, mine.clone()], &run, &trust, host).1,
            Authorship::Trusted
        );

        // A signature by this host's key that no longer holds is not labeled.
        let mut broken = mine;
        broken.signature = sign(&SECRET, &manifest("sha256:else")).signature;
        let (checks, authorship) = assess(&[broken], &run, &nobody, host);
        assert_eq!(authorship, Authorship::Unsigned);
        assert!(!checks[0].this_host);
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
    fn a_handover_names_both_keys_and_checks_out() {
        let handover = hand_over(&SECRET, &OTHER);
        assert_eq!(
            check_handover(&handover),
            Ok((public_key_of(&SECRET), public_key_of(&OTHER)))
        );
    }

    #[test]
    fn a_handover_needs_both_signatures_over_these_two_keys() {
        let third: [u8; 32] = [11; 32];
        // Redirected to a key the old one never named.
        let mut redirected = hand_over(&SECRET, &OTHER);
        redirected.to = public_key_of(&third);
        assert_eq!(check_handover(&redirected), Err(AttestError::BadSignature));
        // Claimed for a key whose holder never signed: the new signature is
        // borrowed from another handover.
        let mut unaccepted = hand_over(&SECRET, &OTHER);
        unaccepted.to_signature = hand_over(&SECRET, &third).to_signature;
        assert_eq!(check_handover(&unaccepted), Err(AttestError::BadSignature));
        // Not vouched for by the old key.
        let mut unvouched = hand_over(&SECRET, &OTHER);
        unvouched.from_signature = hand_over(&third, &OTHER).from_signature;
        assert_eq!(check_handover(&unvouched), Err(AttestError::BadSignature));
        // A run attestation's signature is not a handover signature.
        let mut confused = hand_over(&SECRET, &OTHER);
        confused.from_signature = sign(&SECRET, &manifest("sha256:root")).signature;
        assert_eq!(check_handover(&confused), Err(AttestError::BadSignature));
    }

    #[test]
    fn a_handover_to_itself_or_of_another_kind_is_refused() {
        assert_eq!(
            check_handover(&hand_over(&SECRET, &SECRET)),
            Err(AttestError::SelfHandover)
        );
        let mut other_kind = hand_over(&SECRET, &OTHER);
        other_kind.kind = "flot-key-handover-v0".into();
        assert_eq!(check_handover(&other_kind), Err(AttestError::NotAHandover));
    }

    fn reused(step_id: &str, receipt_id: &str, vouched_by: &[String]) -> RunStep {
        RunStep {
            step_id: step_id.into(),
            receipt_id: receipt_id.into(),
            status: StepStatus::Ok,
            cached: true,
            claims: ClaimCounts::default(),
            observed_digest: String::new(),
            attempts: Vec::new(),
            candidates: Vec::new(),
            vouched_by: vouched_by.to_vec(),
        }
    }

    #[test]
    fn a_signer_is_confirmed_only_by_a_run_it_signed_that_kept_the_same_receipt() {
        let alice = public_key_of(&SECRET);
        let stranger = public_key_of(&OTHER);
        let mut signed = manifest("sha256:signed");
        signed.steps = vec![reused("build", "sha256:r1", &[])];
        let mut ci = manifest("sha256:ci");
        ci.steps = vec![
            reused("build", "sha256:r1", &[alice.clone(), stranger.clone()]),
            // Alice signed this receipt for another step, and this step
            // with another receipt: neither is what she vouched for.
            reused("test", "sha256:r1", std::slice::from_ref(&alice)),
        ];
        let mut other_receipt = manifest("sha256:other");
        other_receipt.steps = vec![reused("build", "sha256:r9", std::slice::from_ref(&alice))];
        let trusted = BTreeMap::from([(alice.clone(), "alice".to_string())]);

        // A run that names alice is not confirmed by naming her, nor by an
        // attestation of hers that does not hold for the run it came with.
        let mut endorsements = Endorsements::default();
        endorsements.add(&[sign(&SECRET, &manifest("sha256:else"))], &signed);
        let unconfirmed = vouched(&ci, &trusted, &endorsements);
        assert_eq!(
            unconfirmed[0].vouched_by[0],
            Voucher {
                key: alice.clone(),
                confirmed: false,
                trusted_as: None,
            }
        );
        assert_eq!(
            unconfirmed[0].vouched_by[0].label(),
            format!("{} (not confirmed here)", fingerprint(&alice))
        );

        endorsements.add(&[sign(&SECRET, &signed)], &signed);
        let judged = vouched(&ci, &trusted, &endorsements);
        assert_eq!(judged.len(), 2);
        assert_eq!(judged[0].vouched_by[0].label(), "alice");
        assert!(!judged[0].vouched_by[1].confirmed, "{judged:?}");
        assert!(!judged[1].vouched_by[0].confirmed, "{judged:?}");
        let elsewhere = vouched(&other_receipt, &trusted, &endorsements);
        assert!(!elsewhere[0].vouched_by[0].confirmed, "{elsewhere:?}");

        // Confirmed, then no longer trusted: named by key, and said so.
        let revoked = vouched(&ci, &BTreeMap::new(), &endorsements);
        assert_eq!(
            revoked[0].vouched_by[0].label(),
            format!("{} (not trusted here)", fingerprint(&alice))
        );
    }

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
