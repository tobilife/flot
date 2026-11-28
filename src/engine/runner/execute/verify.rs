//! Running a step's verify command, in the work or in a copy of it brought
//! to what the work is now, and keeping its output as evidence.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::core::flow::{ResolvedBudget, Step};
use crate::core::metrics::Metrics;
use crate::engine::claims::Proof;
use crate::engine::exec;
use crate::engine::keying;
use crate::engine::runner::{RunOptions, Runner};
use crate::store::Builds;

use super::spend::read_metrics;

impl Runner<'_> {
    /// The copy of the work, brought to what it is now for `step`'s proof.
    /// It is kept from the last proof and only what changed since is written,
    /// stamped past that proof's build, so a build that goes by times
    /// rebuilds only that.
    pub(super) fn copy<'b>(&self, step: &Step, builds: &'b Builds) -> Result<WorkCopy<'b>> {
        let copied = keying::Digests::kept(&builds.digests(), builds.root().to_path_buf());
        keying::sync_workspace(
            &self.root,
            &builds.work(),
            step,
            &self.digests,
            &copied,
            builds.stamp(),
        )
        .context("cannot copy the work")?;
        // What the copy's files held only saves reading them next time.
        let _ = copied.keep(&builds.digests());
        Ok(WorkCopy {
            dir: builds.work(),
            builds,
        })
    }

    /// Run a proof command in `place`, the flow root or a copy of it, and
    /// keep its combined output as evidence. A proof can spend too, an agent
    /// asked to review for instance, so it reports through its own
    /// `$FLOT_METRICS`, counted as the step's spend. Processes it started
    /// that left its process group are added to `escaped`. Each line it
    /// prints is reported as the step's proof's.
    pub(super) fn prove(
        &self,
        step: &Step,
        command: &str,
        place: Place,
        budget: ResolvedBudget,
        options: &RunOptions,
        escaped: &mut Vec<exec::Escaped>,
    ) -> Result<(Proof, Option<Metrics>, Vec<String>)> {
        self.prove_with(step, command, place, &[], budget, options, escaped)
    }

    /// [`Runner::prove`], with `extra` set in the command's environment.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prove_with(
        &self,
        step: &Step,
        command: &str,
        place: Place,
        extra: &[(&str, String)],
        budget: ResolvedBudget,
        options: &RunOptions,
        escaped: &mut Vec<exec::Escaped>,
    ) -> Result<(Proof, Option<Metrics>, Vec<String>)> {
        let (mut env, metrics_path) = self.metered_env(step)?;
        for (name, value) in extra {
            env.insert((*name).into(), value.clone());
        }
        let place = match place {
            Place::Work => self.root.as_path(),
            Place::Copy(copy) => {
                // A copy has no `.git` and sits inside the store: git must
                // not find the real repository above it.
                if let Some(parent) = copy.dir().parent() {
                    env.insert(
                        "GIT_CEILING_DIRECTORIES".into(),
                        parent.to_string_lossy().into_owned(),
                    );
                }
                // Nor may cargo build it where the work builds, as a shared
                // CARGO_TARGET_DIR would have it: cargo goes by its sources'
                // times and paths relative to the package, so the work's
                // next build would take the copy's binaries for its own.
                // The copy builds in one place, held by one proof at a time,
                // and without what only a debugger would use, unless the step
                // asks for it: debug info is most of a build's size. Nor
                // incrementally: on flot itself that is 45% more for a second
                // less after an edit.
                let builds = copy.builds.target().to_string_lossy().into_owned();
                for name in ["CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR"] {
                    env.insert(name.into(), builds.clone());
                }
                for name in [
                    "CARGO_PROFILE_DEV_DEBUG",
                    "CARGO_PROFILE_TEST_DEBUG",
                    "CARGO_INCREMENTAL",
                ] {
                    env.entry(name.into()).or_insert_with(|| "0".into());
                }
                copy.dir()
            }
        };
        let lines = options.progress.lines(&step.id, true);
        let verification = exec::run_confined(
            command,
            place,
            &env,
            exec::Limits::of(budget),
            &options.stop,
            None,
            lines.as_ref(),
        )
        .with_context(|| format!("verify command for step `{}` could not start", step.id))?;
        escaped.extend(verification.escaped.iter().cloned());
        let (metrics, notes) = read_metrics(&metrics_path);
        let notes = notes
            .into_iter()
            .map(|note| format!("verify command: {note}"))
            .collect();
        let exit_code = finished_exit(&verification);
        let mut combined = verification.stdout;
        combined.extend_from_slice(&verification.stderr);
        Ok((
            Proof {
                exit_code,
                timed_out: verification.timed_out,
                output_digest: self.store.put_bytes(&combined)?,
            },
            metrics,
            notes,
        ))
    }
}

/// What a proof exited with, if it ran to its end: one stopped part way,
/// for its output or by a stop, shows nothing whatever it exited with.
fn finished_exit(verification: &exec::Outcome) -> Option<i32> {
    if verification.interrupted || verification.output_overflow {
        None
    } else {
        verification.exit_code
    }
}

/// Where a proof runs.
pub(super) enum Place<'a> {
    /// In the work itself.
    Work,
    /// In a copy of it.
    Copy(&'a WorkCopy<'a>),
}

/// The copy of the work, which a proof can use only while it holds it and
/// the build it goes with.
pub(super) struct WorkCopy<'b> {
    dir: PathBuf,
    builds: &'b Builds,
}

impl WorkCopy<'_> {
    pub(super) fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests;
