//! Where a step's command runs and how: its environment, and the sandbox
//! a step that asked for one runs in.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;

use crate::core::flow::Step;
use crate::engine::runner::Runner;
use crate::engine::sandbox;

use super::scratch::Scratch;
use super::{Setup, Unenforceable};

impl Runner<'_> {
    pub(super) fn step_env(&self, step: &Step) -> BTreeMap<String, String> {
        let mut env = step.env.clone();
        env.insert("FLOT_FLOW".into(), self.flow.meta.name.clone());
        env.insert("FLOT_STEP_ID".into(), step.id.clone());
        if self.candidate.is_some() {
            // The copy has no `.git` and sits inside the store: stop git
            // from finding the real repository above it, so a candidate's
            // git commands cannot touch the workspace it was copied from.
            if let Some(parent) = self.root.parent() {
                env.insert(
                    "GIT_CEILING_DIRECTORIES".into(),
                    parent.to_string_lossy().into_owned(),
                );
            }
        }
        env
    }

    /// Where the step's command runs and how: its environment, the file it
    /// reports its spend in, and its sandbox, which a step that asked for
    /// one runs in or not at all.
    pub(super) fn set_up<'s>(
        &self,
        step: &'s Step,
        extra: &BTreeMap<String, String>,
    ) -> Result<Result<Setup<'s>, Unenforceable>> {
        let (mut env, mut metrics_path) = self.metered_env(step)?;
        env.extend(extra.clone());
        // A confined step writes its metrics and temporary files in a scratch
        // directory of its own, not where other steps' work lies.
        let scratch = match step
            .sandbox
            .as_ref()
            .and_then(|policy| policy.write.as_ref())
        {
            None => None,
            Some(_) => {
                let scratch = Scratch::new(self.store.root(), "sandbox")?;
                metrics_path = scratch.path.join("metrics.json");
                env.insert(
                    "FLOT_METRICS".into(),
                    metrics_path.to_string_lossy().into_owned(),
                );
                env.insert(
                    "TMPDIR".into(),
                    scratch.path.join("tmp").to_string_lossy().into_owned(),
                );
                Some(scratch)
            }
        };
        let confined = match &step.sandbox {
            None => None,
            Some(policy) => {
                let own: Vec<PathBuf> =
                    scratch.iter().map(|scratch| scratch.path.clone()).collect();
                let prepared = policy
                    .write
                    .as_deref()
                    .map(|write| sandbox::resolve(&self.root, write))
                    .transpose()
                    .and_then(|write| sandbox::prepare(policy.network, write.as_deref(), &own));
                match prepared {
                    Ok(prepared) => Some((policy, prepared)),
                    Err(error) => return Ok(Err(Unenforceable(error))),
                }
            }
        };
        Ok(Ok(Setup {
            env,
            metrics_path,
            _scratch: scratch,
            confined,
        }))
    }
}
