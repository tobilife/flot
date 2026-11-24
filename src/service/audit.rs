//! Re-authenticating a recorded run from the store alone.

use anyhow::Result;
use serde::Serialize;

use crate::core::attest::{self, Authorship, SignerCheck};
use crate::core::digest;
use std::collections::BTreeMap;

use crate::core::receipt::{Observed, Receipt, RunManifest, Verdict};
use crate::store::Store;

#[derive(Debug, Clone, Serialize)]
pub struct Audit {
    pub run_id: String,
    pub intact: bool,
    pub root_digest: String,
    pub recomputed_root: String,
    pub verdict: Verdict,
    pub checked_receipts: usize,
    pub findings: Vec<String>,
    /// Who vouched for this run, judged against this store's trust list.
    pub authorship: Authorship,
    pub signers: Vec<SignerCheck>,
}

/// Re-authenticate a run from the store alone: every receipt must hash to its
/// own id, the dependency references must agree with the manifest, every
/// evidence object must still be present, and everything the manifest derives
/// from its receipts (statuses, counts, verdict, root, run id) must follow.
pub fn audit(store: &Store, manifest: &RunManifest) -> Result<Audit> {
    let mut findings = Vec::new();
    let evidence = manifest.evidence();
    // Each receipt with the observation this run recorded for it.
    let mut loaded = Vec::new();
    let mut observations: BTreeMap<&str, Observed> = BTreeMap::new();
    for &(step_id, receipt_id, observed_digest) in &evidence {
        match store.load_receipt(receipt_id) {
            Ok(receipt) => loaded.push((receipt, observed_digest)),
            Err(error) => findings.push(format!("{step_id}: receipt unreadable ({error})")),
        }
        match store.load_observation(observed_digest) {
            Ok(observed) => {
                observations.insert(observed_digest, observed);
            }
            Err(error) => findings.push(format!("{step_id}: observation unreadable ({error:#})")),
        }
    }
    let checked = loaded.len();
    let receipts: BTreeMap<&str, &Receipt> = loaded
        .iter()
        .map(|(receipt, _)| (receipt.receipt_id.as_str(), receipt))
        .collect();

    for (receipt, observed_digest) in &loaded {
        let step_id = &receipt.identity.step_id;
        // Present is not enough: evidence that no longer hashes to its name
        // is not evidence.
        for object in receipt.objects() {
            if let Err(error) = store.read_bytes(object) {
                findings.push(format!(
                    "{}: evidence object {} is not intact ({error:#})",
                    step_id,
                    digest::short(object)
                ));
            }
        }
        let observed = observations.get(observed_digest);
        if let (Some(observed), Ok(stdout), Ok(stderr)) = (
            observed,
            store.read_bytes(&receipt.identity.stdout_digest),
            store.read_bytes(&receipt.identity.stderr_digest),
        ) {
            if !observed.describes(&stdout, &stderr) {
                findings.push(format!(
                    "{}: the output preview does not match the stored output",
                    step_id
                ));
            }
        }
    }
    // Every step whose receipt or observation did not load is reported here
    // as not available, next to the reason recorded above.
    findings.extend(manifest.disagreements(&receipts, &observations));
    let pairs: Vec<(String, String)> = manifest
        .steps
        .iter()
        .map(|step| (step.step_id.clone(), step.receipt_id.clone()))
        .collect();
    let recomputed_root = RunManifest::root_of(&pairs);

    let (signers, authorship) = attest::assess(
        &store.attestations(&manifest.run_id)?,
        manifest,
        &store.trusted_keys()?,
    );
    for signer in signers.iter().filter(|signer| !signer.is_valid()) {
        findings.push(format!(
            "attestation by {} does not hold: {}",
            attest::fingerprint(&signer.key),
            signer.problem.as_deref().unwrap_or("invalid")
        ));
    }

    Ok(Audit {
        run_id: manifest.run_id.clone(),
        intact: findings.is_empty(),
        root_digest: manifest.root_digest.clone(),
        recomputed_root,
        verdict: manifest.verdict,
        checked_receipts: checked,
        findings,
        authorship,
        signers,
    })
}
