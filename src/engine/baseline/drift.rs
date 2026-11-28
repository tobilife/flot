//! A proof compared with the revision: a step of the flow being run, or a
//! receipt a run recorded, and why it differs.

use std::collections::BTreeSet;

use anyhow::Result;

use crate::core::flow::{Mutants, Step};
use crate::core::receipt::{ClaimStatus, FileDigest, ReceiptIdentity};
use crate::engine::keying;

use super::Baseline;

/// Why a step's proof does not match the baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    pub reasons: Vec<String>,
    /// Every claim the step makes here or made at the revision, so a claim
    /// dropped from the flow still shows as unproven.
    pub claims: Vec<String>,
}

/// What a proof is, wherever it is read from: a step of the flow being run,
/// or a receipt a run recorded.
struct Proof<'a> {
    step_id: &'a str,
    claims: Vec<&'a str>,
    verify: Option<&'a str>,
    /// `None` when the source does not record them, as a receipt does not.
    patterns: Option<&'a [String]>,
    files: &'a [FileDigest],
    /// For a step, whether it declares `fails_before`.
    fails_before: Option<bool>,
    /// For a receipt, the commit its proof was run before the change at, if any.
    checked_at: Option<&'a str>,
    /// For a step, the files it holds to the revision.
    held: Option<&'a [String]>,
    /// For a receipt, the digest of the files its proof was held to, if any.
    held_base: Option<&'a str>,
    /// For a step, the mutants it tests its proof against, if any.
    mutants: Option<Option<&'a Mutants>>,
    /// For a receipt, the command that tested its proof against mutants.
    mutated_by: Option<&'a str>,
    /// Whether a claim was verified: only then does how it was matter.
    verified: bool,
}

impl Baseline {
    /// Why `step`'s proof differs from the revision, given the proof inputs
    /// it reads now; `None` when it matches, or when there is nothing to
    /// prove here or there.
    pub fn drift(&self, step: &Step, verify_inputs: &[FileDigest]) -> Result<Option<Drift>> {
        let patterns = step.proof_inputs();
        self.compare(&Proof {
            step_id: &step.id,
            claims: step.claims.iter().map(String::as_str).collect(),
            verify: step.verify.as_deref(),
            patterns: Some(&patterns),
            files: verify_inputs,
            fails_before: Some(step.fails_before),
            checked_at: None,
            held: Some(&step.held),
            held_base: None,
            mutants: Some(step.mutants.as_ref()),
            mutated_by: None,
            verified: false,
        })
    }

    /// Why the proof a receipt recorded differs from the revision. A receipt
    /// does not record its `verify_inputs` patterns, so only the files they
    /// matched are compared.
    pub fn drift_of(&self, identity: &ReceiptIdentity) -> Result<Option<Drift>> {
        self.compare(&Proof {
            step_id: &identity.step_id,
            claims: identity
                .claims
                .iter()
                .map(|claim| claim.text.as_str())
                .collect(),
            verify: identity
                .claims
                .iter()
                .find_map(|claim| claim.verify_command.as_deref()),
            patterns: None,
            files: &identity.verify_inputs,
            fails_before: None,
            checked_at: identity
                .claims
                .iter()
                .find_map(|claim| claim.before.as_ref())
                .map(|before| before.commit.as_str()),
            held: None,
            held_base: identity
                .claims
                .iter()
                .find_map(|claim| claim.held.as_ref())
                .map(|held| held.base.as_str()),
            mutants: None,
            mutated_by: identity
                .claims
                .iter()
                .find_map(|claim| claim.mutants.as_ref())
                .map(|mutated| mutated.command.as_str()),
            verified: identity
                .claims
                .iter()
                .any(|claim| claim.status == ClaimStatus::Verified),
        })
    }

    fn compare(&self, proof: &Proof) -> Result<Option<Drift>> {
        let old = self.flow.as_ref().and_then(|old| old.step(proof.step_id));
        let old_claims: Vec<&str> = old
            .map(|old| old.claims.iter().map(String::as_str).collect())
            .unwrap_or_default();
        if proof.claims.is_empty() && old_claims.is_empty() {
            return Ok(None);
        }
        let mut reasons = Vec::new();
        match old {
            None if self.flow.is_none() => {
                reasons.push("the flow file is not there".to_string());
            }
            None => reasons.push(format!("step `{}` is not there", proof.step_id)),
            Some(old) => {
                if old_claims != proof.claims {
                    reasons.push("the step's claims differ".into());
                }
                if old.verify.as_deref() != proof.verify {
                    reasons.push("its verify command differs".into());
                }
                if proof
                    .patterns
                    .is_some_and(|patterns| patterns != old.proof_inputs().as_slice())
                {
                    reasons.push("its verify_inputs differ".into());
                }
                match proof.fails_before {
                    Some(declared) if declared != old.fails_before => {
                        reasons.push("its fails_before differs".into());
                    }
                    Some(_) => {}
                    // A receipt held to the revision's fails_before: what
                    // it verified, it must have seen fail at the revision.
                    None if old.fails_before
                        && proof.verified
                        && proof.checked_at != Some(self.commit.as_str()) =>
                    {
                        reasons.push(match proof.checked_at {
                            None => "its proof was not run before the change".to_string(),
                            Some(commit) => format!(
                                "its proof was run before the change at {}, not at this revision",
                                &commit[..commit.len().min(12)]
                            ),
                        });
                    }
                    None => {}
                }
                match proof.held {
                    Some(declared) if declared != old.held.as_slice() => {
                        reasons.push("its held files differ".into());
                    }
                    Some(_) => {}
                    // A receipt held to the revision's files: what it
                    // verified, it must have proven with them as they are.
                    None if !old.held.is_empty()
                        && proof.verified
                        && proof.held_base != Some(self.held_base(&old.held).as_str()) =>
                    {
                        reasons.push(match proof.held_base {
                            None => {
                                "its proof was not held to the files at this revision".to_string()
                            }
                            Some(_) => "its proof was held to other files than this revision has"
                                .to_string(),
                        });
                    }
                    None => {}
                }
                match (proof.mutants, &old.mutants) {
                    (Some(declared), _) if declared != old.mutants.as_ref() => {
                        reasons.push("its mutants differ".into());
                    }
                    (Some(_), _) => {}
                    // A receipt tested against the revision's mutants: what
                    // it verified, the revision's command must have tested.
                    (None, Some(mutants)) if proof.verified => match proof.mutated_by {
                        None => reasons.push("its proof was not tested against mutants".into()),
                        Some(command) if command != mutants.run => reasons
                            .push("its proof was tested against mutants by another command".into()),
                        Some(_) => {}
                    },
                    (None, _) => {}
                }
            }
        }
        for file in proof.files {
            match self.digest(&file.path)? {
                None => reasons.push(format!("{} is not there", file.path)),
                Some(digest) if digest != file.digest => {
                    reasons.push(format!("{} differs", file.path))
                }
                Some(_) => {}
            }
        }
        // What the revision says the proof reads, and is not read here.
        let read: BTreeSet<&str> = proof.files.iter().map(|file| file.path.as_str()).collect();
        let patterns: Vec<glob::Pattern> = old
            .iter()
            .flat_map(|old| old.proof_inputs())
            .filter_map(|pattern| glob::Pattern::new(&pattern).ok())
            .collect();
        for path in self.tree.keys() {
            if !read.contains(path.as_str())
                && patterns
                    .iter()
                    .any(|pattern| pattern.matches_with(path, keying::match_options()))
            {
                reasons.push(format!("{path} is there but missing here"));
            }
        }
        if reasons.is_empty() {
            return Ok(None);
        }
        let mut claims: Vec<String> = proof.claims.iter().map(|claim| claim.to_string()).collect();
        for claim in old_claims {
            if !proof.claims.contains(&claim) {
                claims.push(claim.to_string());
            }
        }
        Ok(Some(Drift { reasons, claims }))
    }
}
