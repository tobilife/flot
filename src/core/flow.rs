//! Flow definition: parsing, validation and dependency ordering.
//!
//! A flow is declarative data. It names steps, what each step runs, what files
//! it reads, what it claims, and the command that proves the claim. Nothing
//! here executes anything; `exec` and `run` own that.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Schema version participates in every step key: a change in how flot derives
/// identity must invalidate cached receipts rather than silently reuse them.
pub const SCHEMA_VERSION: u32 = 5;

pub const DEFAULT_TIMEOUT_SECS: u64 = 900;

/// The variable that names the candidate a step runs as. It is part of the
/// candidate's declared environment, so each candidate has its own key.
pub const CANDIDATE_ENV: &str = "FLOT_CANDIDATE";
pub const DEFAULT_MAX_INLINE_BYTES: usize = 4096;
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Admission limit: wall-clock ceiling for one step. Exceeding it stops the
    /// step and is reported as `timeout`, never as success.
    pub timeout_secs: Option<u64>,
    /// Delivery budget: how many output bytes travel inline. The complete
    /// output always stays retrievable from the content store.
    pub max_inline_bytes: Option<usize>,
    /// Admission limit: how many bytes one output stream may produce. A step
    /// past it is stopped and failed; flot holds output in memory and in the
    /// store, so a runaway stream must not be able to exhaust either.
    pub max_output_bytes: Option<usize>,
}

impl Budget {
    fn resolve(&self, parent: &Budget) -> ResolvedBudget {
        ResolvedBudget {
            timeout_secs: self
                .timeout_secs
                .or(parent.timeout_secs)
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
            max_inline_bytes: self
                .max_inline_bytes
                .or(parent.max_inline_bytes)
                .unwrap_or(DEFAULT_MAX_INLINE_BYTES),
            max_output_bytes: self
                .max_output_bytes
                .or(parent.max_output_bytes)
                .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedBudget {
    pub timeout_secs: u64,
    pub max_inline_bytes: usize,
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    /// Command executed through `sh -e -c` from the flow root: the first failing
    /// line fails the step.
    pub run: String,
    #[serde(default)]
    pub needs: Vec<String>,
    /// Glob patterns, relative to the flow root, whose content identity feeds
    /// the step key. Declaring inputs is what makes a cache hit trustworthy.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// What this step asserts once it succeeds. A claim is only ever `verified`
    /// when `verify` exits zero.
    #[serde(default)]
    pub claims: Vec<String>,
    /// Independent command that proves the claims. No verify command means the
    /// claims stay `unverified` and the run cannot be reported as passed.
    #[serde(default)]
    pub verify: Option<String>,
    /// Glob patterns for the files the proof reads, such as an acceptance test.
    /// Their content feeds the step key, so changing the proof re-proves the
    /// claim instead of reusing a verdict the new proof never gave. They are
    /// also guarded: if the step itself changes them, the claim cannot verify.
    #[serde(default)]
    pub verify_inputs: Vec<String>,
    /// Glob patterns for the files the step produces. Each one must match a
    /// file once the step succeeds; their content is recorded in the receipt
    /// and kept in the store, and a reused receipt puts them back.
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub budget: Budget,
    /// When true a nonzero exit does not fail the run, but the step's claims
    /// are still not verified.
    #[serde(default)]
    pub allow_failure: bool,
    /// How many times the step may run in one flot run before it holds: it
    /// succeeds and every claim with a proof command is verified. Each later
    /// attempt sees why the one before did not hold in `$FLOT_FEEDBACK`. Like
    /// a budget, not part of the step key: work that held is the same work
    /// whichever attempt it took.
    #[serde(default)]
    pub attempts: Option<u32>,
    /// Names of interchangeable ways to do the step, such as different agents
    /// or models. Each runs in its own copy of the workspace with
    /// `$FLOT_CANDIDATE` naming it, and is proved there; of those that hold,
    /// the one that reported the lowest cost is kept and its outputs are
    /// written back. The others stay as evidence.
    #[serde(default)]
    pub candidates: Vec<String>,
}

impl Step {
    /// The step as it runs for one candidate: its environment names it.
    pub fn as_candidate(&self, name: &str) -> Step {
        let mut step = self.clone();
        step.env.insert(CANDIDATE_ENV.into(), name.into());
        step
    }

    /// How many attempts one run may make at this step.
    pub fn attempts(&self) -> u32 {
        self.attempts.unwrap_or(1)
    }

    /// Whether `path`, relative to the flow root, is one of this step's
    /// declared outputs. A receipt naming any other file is not this step's to
    /// restore, whatever it claims.
    pub fn declares_output(&self, path: &str) -> bool {
        let options = glob::MatchOptions {
            require_literal_separator: true,
            ..glob::MatchOptions::new()
        };
        is_contained(path)
            && self.outputs.iter().any(|pattern| {
                glob::Pattern::new(pattern).is_ok_and(|pattern| pattern.matches_with(path, options))
            })
    }
}

/// Step ids name receipts in output and are compared across hosts, so they are
/// restricted to a portable, printable alphabet.
pub fn is_valid_step_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A relative path made only of plain components: nothing absolute, no `..`,
/// no `.`. flot writes outputs back to these paths, so they must not escape.
pub fn is_contained(path: &str) -> bool {
    let path = std::path::Path::new(path);
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowMeta {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub budget: Budget,
    /// How many steps may run at once. Concurrency is opt-in because flot can
    /// digest what a step reads but cannot prove what it writes, so only the
    /// flow author knows whether two steps are safe to overlap.
    #[serde(default)]
    pub max_parallel: Option<usize>,
    /// Run cost ceiling in dollars, checked before each step executes against
    /// what executed steps reported through `$FLOT_METRICS`.
    #[serde(default)]
    pub max_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FlowFile {
    flow: FlowMeta,
    #[serde(default, rename = "step")]
    steps: Vec<Step>,
}

#[derive(Debug, Clone)]
pub struct Flow {
    pub meta: FlowMeta,
    pub steps: Vec<Step>,
    index: HashMap<String, usize>,
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

    pub fn step(&self, id: &str) -> Option<&Step> {
        self.index.get(id).map(|position| &self.steps[*position])
    }

    /// Dependency order. Ties break on step id so the order — and therefore the
    /// run root digest — does not depend on declaration order or host threading.
    pub fn topo_order(&self) -> Result<Vec<String>> {
        let mut pending: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for step in &self.steps {
            pending.insert(
                step.id.as_str(),
                step.needs.iter().map(String::as_str).collect(),
            );
        }
        let mut order = Vec::with_capacity(self.steps.len());
        while !pending.is_empty() {
            let ready: Vec<&str> = pending
                .iter()
                .filter(|(_, needs)| needs.is_empty())
                .map(|(id, _)| *id)
                .collect();
            if ready.is_empty() {
                let mut stuck: Vec<&str> = pending.keys().copied().collect();
                stuck.sort();
                bail!("dependency cycle among steps: {}", stuck.join(", "));
            }
            for id in ready {
                pending.remove(id);
                for needs in pending.values_mut() {
                    needs.remove(id);
                }
                order.push(id.to_string());
            }
        }
        Ok(order)
    }

    /// Transitive dependency closure of a step, in dependency order.
    pub fn closure(&self, id: &str) -> Result<Vec<String>> {
        if self.step(id).is_none() {
            bail!("unknown step `{}`", id);
        }
        let wanted: BTreeSet<String> = {
            let mut acc = BTreeSet::new();
            let mut stack = vec![id.to_string()];
            while let Some(current) = stack.pop() {
                if !acc.insert(current.clone()) {
                    continue;
                }
                let step = self.step(&current).expect("validated needs");
                stack.extend(step.needs.iter().cloned());
            }
            acc
        };
        Ok(self
            .topo_order()?
            .into_iter()
            .filter(|candidate| wanted.contains(candidate))
            .collect())
    }

    pub fn resolved_budget(&self, step: &Step) -> ResolvedBudget {
        step.budget.resolve(&self.meta.budget)
    }

    /// Identity of the flow definition itself, independent of declaration order.
    pub fn digest(&self) -> String {
        let mut steps: Vec<&Step> = self.steps.iter().collect();
        steps.sort_by(|a, b| a.id.cmp(&b.id));
        crate::core::digest::canonical(&serde_json::json!({
            "schema": SCHEMA_VERSION,
            "name": self.meta.name,
            "steps": steps,
        }))
    }

    /// Steps that declare claims without a verify command. These are reported,
    /// not refused: an unproven claim is a known limitation of the run.
    pub fn unproven_claim_steps(&self) -> Vec<&Step> {
        self.steps
            .iter()
            .filter(|step| !step.claims.is_empty() && step.verify.is_none())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(text: &str) -> Result<Flow> {
        Flow::parse(text)
    }

    const BASIC: &str = r#"
[flow]
name = "demo"

[[step]]
id = "b"
run = "echo b"
needs = ["a"]

[[step]]
id = "a"
run = "echo a"
"#;

    #[test]
    fn orders_dependencies_before_dependents() {
        let flow = flow(BASIC).unwrap();
        assert_eq!(flow.topo_order().unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn rejects_cycles() {
        let err = flow(
            r#"
[flow]
name = "cyclic"

[[step]]
id = "a"
run = "echo a"
needs = ["b"]

[[step]]
id = "b"
run = "echo b"
needs = ["a"]
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("dependency cycle"), "{err}");
    }

    #[test]
    fn rejects_unknown_need() {
        let err = flow(
            r#"
[flow]
name = "x"

[[step]]
id = "a"
run = "echo a"
needs = ["ghost"]
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown step `ghost`"), "{err}");
    }

    #[test]
    fn rejects_duplicate_ids() {
        let err = flow(
            r#"
[flow]
name = "x"

[[step]]
id = "a"
run = "echo 1"

[[step]]
id = "a"
run = "echo 2"
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("more than once"), "{err}");
    }

    #[test]
    fn flow_digest_ignores_declaration_order() {
        let reordered = r#"
[flow]
name = "demo"

[[step]]
id = "a"
run = "echo a"

[[step]]
id = "b"
run = "echo b"
needs = ["a"]
"#;
        assert_eq!(
            flow(BASIC).unwrap().digest(),
            flow(reordered).unwrap().digest()
        );
    }

    #[test]
    fn flow_digest_tracks_command_changes() {
        let changed = BASIC.replace("echo b", "echo c");
        assert_ne!(
            flow(BASIC).unwrap().digest(),
            flow(&changed).unwrap().digest()
        );
    }

    #[test]
    fn only_plain_relative_paths_are_contained() {
        for path in ["a.txt", "build/out.txt", "a/b/c"] {
            assert!(is_contained(path), "{path}");
        }
        for path in ["", "../a", "a/../../b", "/etc/passwd", "./a"] {
            assert!(!is_contained(path), "{path}");
        }
    }

    #[test]
    fn a_step_declares_only_files_its_patterns_match() {
        let text = r#"
[flow]
name = "o"

[[step]]
id = "s"
run = "true"
outputs = ["build/*.txt", "dist/**/*.js", "REPORT.md"]
"#;
        let flow = flow(text).unwrap();
        let step = flow.step("s").unwrap();
        for path in ["build/a.txt", "dist/x.js", "dist/a/b/x.js", "REPORT.md"] {
            assert!(step.declares_output(path), "{path}");
        }
        for path in [
            "build/sub/a.txt",
            "build/a.md",
            "other.txt",
            "../REPORT.md",
            "dist/../REPORT.md",
        ] {
            assert!(!step.declares_output(path), "{path}");
        }
    }

    #[test]
    fn closure_keeps_only_transitive_dependencies() {
        let text = r#"
[flow]
name = "demo"

[[step]]
id = "a"
run = "echo a"

[[step]]
id = "b"
run = "echo b"
needs = ["a"]

[[step]]
id = "side"
run = "echo side"
"#;
        let flow = flow(text).unwrap();
        assert_eq!(flow.closure("b").unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn budget_falls_back_to_flow_then_default() {
        let text = r#"
[flow]
name = "demo"
budget = { timeout_secs = 5 }

[[step]]
id = "a"
run = "echo a"

[[step]]
id = "b"
run = "echo b"
budget = { timeout_secs = 9, max_inline_bytes = 16 }
"#;
        let flow = flow(text).unwrap();
        let a = flow.resolved_budget(flow.step("a").unwrap());
        assert_eq!(a.timeout_secs, 5);
        assert_eq!(a.max_inline_bytes, DEFAULT_MAX_INLINE_BYTES);
        let b = flow.resolved_budget(flow.step("b").unwrap());
        assert_eq!(b.timeout_secs, 9);
        assert_eq!(b.max_inline_bytes, 16);
    }

    #[test]
    fn reports_claims_that_have_no_verify_command() {
        let text = r#"
[flow]
name = "demo"

[[step]]
id = "proven"
run = "echo a"
claims = ["a holds"]
verify = "true"

[[step]]
id = "unproven"
run = "echo b"
claims = ["b holds"]
"#;
        let flow = flow(text).unwrap();
        let ids: Vec<&str> = flow
            .unproven_claim_steps()
            .iter()
            .map(|step| step.id.as_str())
            .collect();
        assert_eq!(ids, vec!["unproven"]);
    }

    #[test]
    fn a_misspelled_key_is_refused_not_ignored() {
        let base = "[flow]\nname = \"demo\"\n\n[[step]]\nid = \"a\"\nrun = \"true\"\n";
        assert!(flow(base).is_ok());
        for (extra, key) in [
            ("verfy = \"false\"\n", "verfy"),
            ("input = [\"src/**\"]\n", "input"),
            ("budget = { timeout = 5 }\n", "timeout"),
        ] {
            let error = format!("{:#}", flow(&format!("{base}{extra}")).unwrap_err());
            assert!(error.contains(key), "{key}: {error}");
        }
        let meta =
            "[flow]\nname = \"demo\"\nmax_paralel = 2\n\n[[step]]\nid = \"a\"\nrun = \"true\"\n";
        assert!(format!("{:#}", flow(meta).unwrap_err()).contains("max_paralel"));
        let top = format!("{base}\n[steps]\nid = \"b\"\n");
        assert!(format!("{:#}", flow(&top).unwrap_err()).contains("steps"));
    }
}
