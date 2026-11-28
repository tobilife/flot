//! Tests of how the attestations of a run establish its authorship.

use std::collections::BTreeMap;

use super::super::{assess, sign, Authorship};
use super::{manifest, OTHER, SECRET};

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
