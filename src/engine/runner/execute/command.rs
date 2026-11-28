//! Running a step's command: alone or beside other steps, and between
//! snapshots of the workspace when what it changes matters.

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use crate::core::flow::{ResolvedBudget, Step};
use crate::core::receipt::FileDigest;
use crate::engine::exec;
use crate::engine::keying;
use crate::engine::runner::{RunOptions, Runner};

use super::{Ran, Setup, Unenforceable};

impl Runner<'_> {
    /// Run the step's command: alone when it declares `may_change`, so the
    /// files that changed while it ran are its own doing, and between two
    /// snapshots of the workspace when what it changes matters. Each line it
    /// prints is reported as it prints it.
    pub(super) fn run_step(
        &self,
        step: &Step,
        setup: &Setup<'_>,
        budget: ResolvedBudget,
        options: &RunOptions,
        since: &mut Option<Vec<FileDigest>>,
    ) -> Result<Result<Ran, Unenforceable>> {
        let scoped = !step.may_change.is_empty();
        // A poisoned lock guards nothing: the data is `()`.
        let (_shared, _alone) = if scoped {
            let alone = self
                .scope
                .write()
                .unwrap_or_else(|poison| poison.into_inner());
            (None, Some(alone))
        } else {
            let shared = self
                .scope
                .read()
                .unwrap_or_else(|poison| poison.into_inner());
            (Some(shared), None)
        };
        let binds = self.binds();
        let before =
            (scoped || binds).then(|| keying::snapshot(&self.root, &self.origin, &self.digests));
        let lines = options.progress.lines(&step.id, false);
        let outcome = match exec::run_confined(
            &step.run,
            &self.root,
            &setup.env,
            exec::Limits::of(budget),
            &options.stop,
            setup.confined.as_ref().map(|(_, prepared)| prepared),
            lines.as_ref(),
        ) {
            Ok(outcome) => outcome,
            // Confinement is applied as the step starts, so a kernel that
            // refuses it shows here.
            Err(error) if setup.confined.is_some() => return Ok(Err(Unenforceable(error))),
            Err(error) => {
                return Err(error).with_context(|| format!("step `{}` could not start", step.id));
            }
        };
        let after = before
            .as_ref()
            .map(|_| keying::snapshot(&self.root, &self.origin, &self.digests));
        let mut problems = Vec::new();
        let strayed = match (&before, &after) {
            (Some(before), Some(after)) if scoped => {
                problems.extend(before.problems.iter().cloned());
                problems.extend(after.problems.iter().cloned());
                Some(keying::out_of_scope(step, &before.files, &after.files))
            }
            _ => None,
        };
        drop((_shared, _alone));
        let changed = match (before, after) {
            (Some(before), Some(after)) if binds => {
                keying::changes(since.get_or_insert(before.files), &after.files)
            }
            _ => BTreeMap::new(),
        };
        Ok(Ok(Ran {
            outcome,
            problems,
            strayed,
            changed,
        }))
    }
}
