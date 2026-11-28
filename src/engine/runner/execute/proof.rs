//! Settling a step's claims on what its proof shows, then on what it does
//! before the change, with the held files, and against mutants.

use crate::core::flow::ResolvedBudget;
use crate::core::receipt::{ClaimStatus, FileDigest};
use crate::engine::claims::{self, ProofOutcome};
use crate::engine::keying::{self, Facts};
use crate::engine::runner::{Keyed, RunOptions, Runner};

use super::verify::Place;
use super::Findings;

impl Runner<'_> {
    /// Settle the step's claims on what its proof shows, and for a proof
    /// that must fail before the change, on what it does there. A process
    /// that left its group meanwhile leaves them unverified.
    pub(super) fn settle_claims(
        &self,
        keyed: &Keyed<'_>,
        after: &Facts,
        files: &[FileDigest],
        budget: ResolvedBudget,
        options: &RunOptions,
        found: &mut Findings,
    ) {
        let step = keyed.step;
        let proof = self.proof(keyed, after, files, budget, options, found);
        let mut claims = claims::settle(
            step,
            found.status,
            found.tolerated,
            proof,
            &mut found.limitations,
        );
        if step.fails_before
            && claims
                .iter()
                .any(|claim| claim.status == ClaimStatus::Verified)
        {
            let (red, red_metrics, red_notes) = self.red(step, budget, options, &mut found.escaped);
            found.spent(red_metrics, red_notes);
            claims = claims::hold_before(claims, red, &mut found.limitations);
        }
        if !step.held.is_empty()
            && claims
                .iter()
                .any(|claim| claim.status == ClaimStatus::Verified)
        {
            let (hold, hold_metrics, hold_notes) =
                self.held(step, budget, options, &mut found.escaped);
            found.spent(hold_metrics, hold_notes);
            claims = claims::hold_held(&step.id, claims, hold, &mut found.limitations);
        }
        if step.mutants.is_some()
            && claims
                .iter()
                .any(|claim| claim.status == ClaimStatus::Verified)
        {
            let (mutation, mutation_metrics, mutation_notes) =
                self.mutated(step, after, files, budget, options, &mut found.escaped);
            found.spent(mutation_metrics, mutation_notes);
            claims = claims::hold_mutants(&step.id, claims, mutation, &mut found.limitations);
        }
        found.claims = claims;
        found.escapes();
    }

    /// What the step's proof shows: run when the step succeeded with claims
    /// to prove, against the proof inputs as they were before the step ran
    /// and the outputs as recorded, and only if those are still what it
    /// judged once it is done.
    fn proof(
        &self,
        keyed: &Keyed<'_>,
        after: &Facts,
        files: &[FileDigest],
        budget: ResolvedBudget,
        options: &RunOptions,
        found: &mut Findings,
    ) -> ProofOutcome {
        let (step, facts) = (keyed.step, &keyed.facts);
        let drift = self.drift(step, &facts.verify_inputs);
        let command = match (
            drift,
            claims::proof_command(step, found.status, found.tolerated),
        ) {
            (Some(drift), _) => return drift,
            (None, None) => return ProofOutcome::NotRun,
            (None, Some(command)) => command,
        };
        let changed = keying::changed_paths(&facts.verify_inputs, &after.verify_inputs);
        if !changed.is_empty() {
            return ProofOutcome::Compromised { changed };
        }
        match self.prove(
            step,
            command,
            Place::Work,
            budget,
            options,
            &mut found.escaped,
        ) {
            Err(error) => ProofOutcome::Failed(format!("{error:#}")),
            Ok((proof, proof_metrics, proof_notes)) => {
                found.spent(proof_metrics, proof_notes);
                // A verdict holds only for the files it judged: the outputs
                // as recorded and the proof inputs as they were before it ran.
                let mut changed = self.changed_outputs(step, files);
                changed.extend(keying::changed_paths(
                    &after.verify_inputs,
                    &self.facts(step).verify_inputs,
                ));
                if changed.is_empty() {
                    ProofOutcome::Ran(proof)
                } else {
                    changed.sort();
                    changed.dedup();
                    ProofOutcome::Disturbed { changed }
                }
            }
        }
    }
}
