//! Reading a flow file, and refusing one flot cannot run as written: an id
//! or path it cannot use, a need that is unknown, repeated or circular, or
//! a setting without what it needs.

use std::collections::{BTreeSet, HashMap};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::{is_contained, is_valid_step_id, Flow, FlowMeta, Step, CANDIDATE_ENV};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FlowFile {
    flow: FlowMeta,
    #[serde(default, rename = "step")]
    steps: Vec<Step>,
}

impl Flow {
    pub fn parse(text: &str) -> Result<Self> {
        let file: FlowFile =
            toml::from_str(text).context("the flow file is not a valid flot flow")?;
        let flow = Flow {
            meta: file.flow,
            steps: file.steps,
            index: HashMap::new(),
        };
        flow.validated()
    }

    fn validated(mut self) -> Result<Self> {
        if self.meta.name.trim().is_empty() {
            bail!("flow.name must not be empty");
        }
        if self.steps.is_empty() {
            bail!("flow `{}` declares no steps", self.meta.name);
        }
        if self
            .meta
            .proofs_from
            .as_deref()
            .is_some_and(|rev| rev.trim().is_empty() || rev.starts_with('-'))
        {
            bail!("flow.proofs_from must name a git revision");
        }
        if let Some(ceiling) = self.meta.max_cost_usd {
            crate::core::metrics::usd_to_micro(ceiling)
                .with_context(|| format!("flow.max_cost_usd `{ceiling}` is invalid"))?;
        }
        if self.meta.max_parallel == Some(0) {
            bail!("flow.max_parallel must be at least 1");
        }
        let mut index = HashMap::new();
        for (position, step) in self.steps.iter().enumerate() {
            if step.id.trim().is_empty() {
                bail!("step #{} has an empty id", position + 1);
            }
            if !is_valid_step_id(&step.id) {
                bail!(
                    "step id `{}` must use only ASCII letters, digits, `-` or `_`",
                    step.id
                );
            }
            if step.run.trim().is_empty() {
                bail!("step `{}` has an empty run command", step.id);
            }
            if step.attempts == Some(0) {
                bail!("step `{}` attempts must be at least 1", step.id);
            }
            if step.fails_before {
                if step.claims.is_empty() || step.verify.is_none() {
                    bail!(
                        "step `{}` sets fails_before, which needs claims and a verify command to prove them",
                        step.id
                    );
                }
                if !step.candidates.is_empty() {
                    bail!(
                        "step `{}` sets fails_before, which candidates do not support: a candidate's copy of the work has no history to undo its changes against",
                        step.id
                    );
                }
            }
            if !step.held.is_empty() {
                if step.claims.is_empty() || step.verify.is_none() {
                    bail!(
                        "step `{}` holds files to the revision, which needs claims and a verify command to prove them",
                        step.id
                    );
                }
                if !step.candidates.is_empty() {
                    bail!(
                        "step `{}` holds files to the revision, which candidates do not support: each candidate's copy would need its own",
                        step.id
                    );
                }
                for pattern in &step.held {
                    if !is_contained(pattern) {
                        bail!(
                            "step `{}` held `{}` must be a relative path inside the flow root",
                            step.id,
                            pattern
                        );
                    }
                    glob::Pattern::new(pattern).with_context(|| {
                        format!("step `{}` held `{}` is not a valid glob", step.id, pattern)
                    })?;
                }
            }
            if let Some(mutants) = &step.mutants {
                if step.claims.is_empty() || step.verify.is_none() {
                    bail!(
                        "step `{}` tests mutants, which needs claims and a verify command to prove them",
                        step.id
                    );
                }
                if !step.candidates.is_empty() {
                    bail!(
                        "step `{}` tests mutants, which candidates do not support: the mutants are of the work, not of a candidate's copy",
                        step.id
                    );
                }
                if mutants.run.trim().is_empty() {
                    bail!("step `{}` has an empty mutants run command", step.id);
                }
                if !is_contained(&mutants.report) {
                    bail!(
                        "step `{}` mutants report `{}` must be a relative path inside $FLOT_MUTANTS_DIR",
                        step.id,
                        mutants.report
                    );
                }
                if let Some(allow) = &mutants.allow {
                    if !is_contained(allow) || allow.contains(['*', '?', '[']) {
                        bail!(
                            "step `{}` mutants allow `{}` must be a path inside the flow root, without wildcards",
                            step.id,
                            allow
                        );
                    }
                    if step.declares_output(allow) {
                        bail!(
                            "step `{}` declares its mutants allow list `{}` as an output; a step cannot produce its own proof",
                            step.id,
                            allow
                        );
                    }
                }
            }
            if step.always && !step.candidates.is_empty() {
                bail!(
                    "step `{}` sets always, which candidates do not support: every candidate would run on every run, and which one is kept could change with what each reported spending, so the steps that need it could not be reused",
                    step.id
                );
            }
            if !step.candidates.is_empty() {
                if step.candidates.len() < 2 {
                    bail!(
                        "step `{}` names one candidate; candidates compare two or more",
                        step.id
                    );
                }
                let mut names = BTreeSet::new();
                for name in &step.candidates {
                    if !is_valid_step_id(name) {
                        bail!(
                            "step `{}` candidate `{}` must use only ASCII letters, digits, `-` or `_`",
                            step.id,
                            name
                        );
                    }
                    if !names.insert(name) {
                        bail!("step `{}` names candidate `{}` twice", step.id, name);
                    }
                }
                if step.env.contains_key(CANDIDATE_ENV) {
                    bail!(
                        "step `{}` sets {CANDIDATE_ENV}, which flot sets to name each candidate",
                        step.id
                    );
                }
            }
            if !step.verify_inputs.is_empty() && step.verify.is_none() {
                bail!(
                    "step `{}` declares verify_inputs without a verify command that reads them",
                    step.id
                );
            }
            if let Some(sandbox) = &step.sandbox {
                if sandbox.network && sandbox.write.is_none() {
                    bail!(
                        "step `{}` has a sandbox that restricts nothing; set `network = false` or `write`",
                        step.id
                    );
                }
                for path in sandbox.write.iter().flatten() {
                    if !is_contained(path) || path.contains(['*', '?', '[']) {
                        bail!(
                            "step `{}` sandbox write `{}` must be a path inside the flow root, without wildcards",
                            step.id,
                            path
                        );
                    }
                }
            }
            for pattern in &step.may_change {
                if !is_contained(pattern) {
                    bail!(
                        "step `{}` may_change `{}` must be a relative path inside the flow root",
                        step.id,
                        pattern
                    );
                }
                glob::Pattern::new(pattern).with_context(|| {
                    format!(
                        "step `{}` may_change `{}` is not a valid glob",
                        step.id, pattern
                    )
                })?;
            }
            for pattern in &step.outputs {
                if !is_contained(pattern) {
                    bail!(
                        "step `{}` output `{}` must be a relative path inside the flow root",
                        step.id,
                        pattern
                    );
                }
                glob::Pattern::new(pattern).with_context(|| {
                    format!(
                        "step `{}` output `{}` is not a valid glob",
                        step.id, pattern
                    )
                })?;
                if step.verify_inputs.contains(pattern) {
                    bail!(
                        "step `{}` declares `{}` as both an output and a proof input; a step cannot produce its own proof",
                        step.id,
                        pattern
                    );
                }
            }
            if index.insert(step.id.clone(), position).is_some() {
                bail!("step id `{}` is declared more than once", step.id);
            }
        }
        for step in &self.steps {
            let mut seen = BTreeSet::new();
            for need in &step.needs {
                if !index.contains_key(need) {
                    bail!("step `{}` needs unknown step `{}`", step.id, need);
                }
                if need == &step.id {
                    bail!("step `{}` needs itself", step.id);
                }
                if !seen.insert(need.clone()) {
                    bail!("step `{}` lists need `{}` twice", step.id, need);
                }
            }
        }
        self.index = index;
        // Reject cycles up front so every later pass can assume a DAG.
        self.topo_order()?;
        Ok(self)
    }
}
