//! Re-authenticating a recorded run from the store alone.

use anyhow::Result;
use serde::Serialize;

use crate::core::attest::{self, Authorship, Endorsements, SignerCheck, VouchedStep};
use crate::core::digest;
use std::collections::{BTreeMap, BTreeSet};

use crate::core::receipt::{Observed, Receipt, RunManifest, Verdict};
use crate::engine::baseline::Baseline;
use crate::service::keys;
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
    /// Steps the run reused from imported runs, with who vouched for each,
    /// as [`vouched`] judges them. The run is unsigned when CI reuses a
    /// developer's signed run; this says whose word it rests on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub vouched: Vec<VouchedStep>,
    /// How the run's proofs compare with a revision, when checked against one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
}

/// A recorded run's proofs held to a git revision.
#[derive(Debug, Clone, Serialize)]
pub struct Anchor {
    /// The revision and the commit it named, such as `origin/main (1a2b…)`.
    pub baseline: String,
    /// Steps whose recorded proof differs from the revision.
    pub drifted: Vec<StepDrift>,
    /// Steps with claims at the revision that the run did not record.
    pub missing: Vec<String>,
    /// Files that differ here from the revision and that no step of the run
    /// read, checked or produced: changes no claim speaks for.
    pub unread: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepDrift {
    pub step_id: String,
    pub reasons: Vec<String>,
}

impl Anchor {
    /// Whether every claim the revision makes was proven as it says.
    pub fn holds(&self) -> bool {
        self.drifted.is_empty() && self.missing.is_empty()
    }
}

/// Compare each receipt `manifest` kept with `baseline`. A receipt that
/// cannot be read is left to the integrity findings.
pub fn anchor(store: &Store, manifest: &RunManifest, baseline: &Baseline) -> Result<Anchor> {
    let mut drifted = Vec::new();
    let mut read: BTreeSet<String> = BTreeSet::new();
    for step in &manifest.steps {
        let Ok(receipt) = store.load_receipt(&step.receipt_id) else {
            continue;
        };
        let identity = &receipt.identity;
        for file in identity
            .inputs
            .iter()
            .chain(&identity.verify_inputs)
            .chain(&identity.outputs)
        {
            read.insert(file.path.clone());
        }
        if let Some(drift) = baseline.drift_of(&receipt.identity)? {
            drifted.push(StepDrift {
                step_id: step.step_id.clone(),
                reasons: drift.reasons,
            });
        }
    }
    let missing = baseline.missing_steps(|id| manifest.steps.iter().any(|step| step.step_id == id));
    let unread = baseline.unread(&read)?;
    Ok(Anchor {
        baseline: baseline.label(),
        drifted,
        missing,
        unread,
    })
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

    let (signers, authorship) = authorship(store, manifest)?;
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
        vouched: vouched(store, manifest)?,
        anchor: None,
    })
}

/// Who signed `manifest`, judged against this store's trust list and the
/// signing key this host holds. The host key only labels its own signature;
/// it is never a reason to trust or reuse a run.
pub fn authorship(store: &Store, manifest: &RunManifest) -> Result<(Vec<SignerCheck>, Authorship)> {
    Ok(attest::assess(
        &store.attestations(&manifest.run_id)?,
        manifest,
        &store.trusted_keys()?,
        keys::host_public_key().as_deref(),
    ))
}

/// Who vouched for each step `manifest` reused from an imported run. The
/// signers a run records for a step are its own word, which nothing covers in
/// an unsigned run and which an imported run can make up. So a signer counts
/// as confirmed only when this store holds a run the signer validly signed
/// that kept the same receipt for the step, and is named only when this
/// store also trusts it. A run that names no signer reads nothing.
pub fn vouched(store: &Store, manifest: &RunManifest) -> Result<Vec<VouchedStep>> {
    if manifest.steps.iter().all(|step| step.vouched_by.is_empty()) {
        return Ok(Vec::new());
    }
    let mut endorsements = Endorsements::default();
    for run_id in store.list_runs()? {
        // A record that cannot be read confirms nothing.
        let Ok(attestations) = store.attestations(&run_id) else {
            continue;
        };
        if attestations.is_empty() {
            continue;
        }
        if let Ok(signed) = store.resolve_run(&run_id) {
            endorsements.add(&attestations, &signed);
        }
    }
    Ok(attest::vouched(
        manifest,
        &store.trusted_keys()?,
        &endorsements,
    ))
}
