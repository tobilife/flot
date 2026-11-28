//! Tests of key handovers: both keys sign it, and over these two keys.

use ed25519_dalek::Signature;

use super::super::{check_handover, hand_over, parse_key, public_key_of, sign, AttestError};
use super::{manifest, OTHER, SECRET};

#[test]
fn a_handover_names_both_keys_and_checks_out() {
    let handover = hand_over(&SECRET, &OTHER);
    assert_eq!(
        check_handover(&handover),
        Ok((public_key_of(&SECRET), public_key_of(&OTHER)))
    );
}

/// The exact bytes both keys sign: the handover kind, the old key and the
/// new key, one per line, so a signature names the pair it hands over.
#[test]
fn both_keys_sign_the_kind_and_the_two_keys_one_per_line() {
    let handover = hand_over(&SECRET, &OTHER);
    let statement = format!(
        "flot-key-handover-v1\n{}\n{}",
        public_key_of(&SECRET),
        public_key_of(&OTHER)
    );
    for (key, signature) in [
        (&handover.from, &handover.from_signature),
        (&handover.to, &handover.to_signature),
    ] {
        let bytes: [u8; 64] = hex::decode(signature).unwrap().try_into().unwrap();
        assert!(
            parse_key(key)
                .unwrap()
                .verify_strict(statement.as_bytes(), &Signature::from_bytes(&bytes))
                .is_ok(),
            "{key} did not sign {statement:?}"
        );
    }
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
    // Both keys signed, each over another handover: the old key's word for
    // one successor is not its word for another.
    let mut transferred = hand_over(&SECRET, &third);
    transferred.from_signature = hand_over(&SECRET, &OTHER).from_signature;
    assert_eq!(check_handover(&transferred), Err(AttestError::BadSignature));
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
