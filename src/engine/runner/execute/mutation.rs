//! Testing a step's proof against mutants: the mutation run, and its report
//! judged against the allow list the proof read.

use std::path::Path;

use anyhow::{Context, Result};

use crate::core::digest;
use crate::core::flow::{Mutants, ResolvedBudget, Step};
use crate::core::metrics::Metrics;
use crate::core::mutants;
use crate::core::receipt::FileDigest;
use crate::engine::claims::{Judged, Mutation};
use crate::engine::exec;
use crate::engine::keying::{self, Facts};
use crate::engine::runner::{RunOptions, Runner};

use super::scratch::Scratch;
use super::verify::Place;

impl Runner<'_> {
    /// What `step`'s mutation tester finds of its proof: run from the flow
    /// root with `$FLOT_MUTANTS_DIR`, an empty directory for its report, and
    /// `$FLOT_PROOFS_FROM`, the commit proofs are checked against, when there
    /// is one. Its report is judged against the allow list as the proof read
    /// it, and only while the run left the files the verdict judged as they
    /// were: the outputs as recorded and the proof inputs as `after` has
    /// them. That run counts as the step's spend.
    pub(super) fn mutated(
        &self,
        step: &Step,
        after: &Facts,
        files: &[FileDigest],
        budget: ResolvedBudget,
        options: &RunOptions,
        escaped: &mut Vec<exec::Escaped>,
    ) -> (Mutation, Option<Metrics>, Vec<String>) {
        let mutants = step
            .mutants
            .as_ref()
            .expect("a step that tests mutants declares them");
        let ran = (|| -> Result<_> {
            let scratch = Scratch::new(self.store.root(), "mutants")?;
            let dir = scratch.path.join("out");
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
            let commit = self.baseline.map(|baseline| baseline.commit.clone());
            let mut extra = vec![("FLOT_MUTANTS_DIR", dir.to_string_lossy().into_owned())];
            if let Some(commit) = &commit {
                extra.push(("FLOT_PROOFS_FROM", commit.clone()));
            }
            let (run, metrics, notes) = self.prove_with(
                step,
                &mutants.run,
                Place::Work,
                &extra,
                budget,
                options,
                escaped,
            )?;
            let mut changed = self.changed_outputs(step, files);
            changed.extend(keying::changed_paths(
                &after.verify_inputs,
                &self.facts(step).verify_inputs,
            ));
            changed.sort();
            changed.dedup();
            let report_path = dir.join(&mutants.report);
            let (report_digest, judged) = match std::fs::read(&report_path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let how = match run.exit_code {
                        Some(0) => "succeeded".to_string(),
                        Some(code) => format!("exited {code}"),
                        None => "was stopped".to_string(),
                    };
                    (
                        None,
                        Err(format!(
                            "the run {how} and wrote no report at $FLOT_MUTANTS_DIR/{}",
                            mutants.report
                        )),
                    )
                }
                Err(error) => (
                    None,
                    Err(format!(
                        "cannot read $FLOT_MUTANTS_DIR/{}: {error}",
                        mutants.report
                    )),
                ),
                Ok(report) => (
                    Some(self.store.put_bytes(&report)?),
                    self.judge(mutants, &report, &report_path, after)
                        .map_err(|error| format!("{error:#}")),
                ),
            };
            Ok((
                Mutation::Ran {
                    command: mutants.run.clone(),
                    commit,
                    run,
                    report_digest,
                    judged,
                    changed,
                },
                metrics,
                notes,
            ))
        })();
        match ran {
            Ok((mutation, metrics, notes)) => (
                mutation,
                metrics,
                notes
                    .into_iter()
                    .map(|note| format!("testing mutants, {note}"))
                    .collect(),
            ),
            Err(error) => (Mutation::Unchecked(format!("{error:#}")), None, Vec::new()),
        }
    }

    /// The mutation report at `path`, its survivors split by the allow list
    /// the proof read, as `after` records it.
    fn judge(
        &self,
        mutants: &Mutants,
        report: &[u8],
        path: &Path,
        after: &Facts,
    ) -> Result<Judged> {
        // cargo-mutants lists the mutants it was to test beside its outcomes.
        let planned = path
            .parent()
            .map(|dir| dir.join("mutants.json"))
            .and_then(|planned| std::fs::read(planned).ok());
        let root = std::fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        let source = |file: &str| -> Option<String> {
            if !crate::core::flow::is_contained(file) {
                return None;
            }
            std::fs::read_to_string(self.root.join(file)).ok()
        };
        let read = mutants::read_report(report, planned.as_deref(), &source, &root)?;
        let allowed = match &mutants.allow {
            None => Vec::new(),
            Some(allow) => match after.verify_inputs.iter().find(|file| &file.path == allow) {
                // No allow list: nothing is allowed.
                None => Vec::new(),
                Some(file) => {
                    let text = std::fs::read(self.root.join(allow))
                        .with_context(|| format!("cannot read {allow}"))?;
                    if digest::bytes(&text) != file.digest {
                        anyhow::bail!("{allow} changed since the proof ran");
                    }
                    let text =
                        String::from_utf8(text).with_context(|| format!("{allow} is not UTF-8"))?;
                    mutants::read_allow(&text).with_context(|| format!("in {allow}"))?
                }
            },
        };
        let (survived, allowed) = mutants::judge(read.survived, &allowed);
        Ok(Judged {
            caught: read.caught,
            untested: read.untested,
            survived,
            allowed,
        })
    }
}
