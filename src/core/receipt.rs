//! Receipts and run manifests.
//!
//! A receipt is the only thing flot trusts. It splits cleanly in two:
//!
//! * `identity` — the deterministic facts of a step: what ran, over which
//!   inputs, after which dependencies, with what exit code, output digests and
//!   claim verdicts. The receipt id is the digest of exactly this body.
//! * `observed` — host and wall-clock context plus inline output previews.
//!   Never hashed, because two honest runs of the same work disagree on timing.
//!
//! That split is what makes `flot diff` meaningful: identical identity digests
//! mean two runs did the same work and reached the same verdict.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::core::digest;
use crate::core::flow::SCHEMA_VERSION;
use crate::core::metrics::{Metrics, Spend};

pub const FLOT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// Command ran and exited zero.
    Ok,
    /// Command ran and exited nonzero, or exited zero without producing a
    /// declared output.
    Failed,
    /// Admission limit stopped the command before it finished.
    Timeout,
    /// Not run because a dependency did not reach `Ok`.
    Cancelled,
}

impl StepStatus {
    pub fn is_ok(self) -> bool {
        matches!(self, StepStatus::Ok)
    }

    pub fn label(self) -> &'static str {
        match self {
            StepStatus::Ok => "ok",
            StepStatus::Failed => "failed",
            StepStatus::Timeout => "timeout",
            StepStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    /// An independent verify command exited zero after the step succeeded.
    Verified,
    /// Nothing proved the claim. This is the default, and it is not a success.
    Unverified,
    /// A verify command ran and rejected the claim.
    Refuted,
}

impl ClaimStatus {
    pub fn label(self) -> &'static str {
        match self {
            ClaimStatus::Verified => "verified",
            ClaimStatus::Unverified => "unverified",
            ClaimStatus::Refuted => "refuted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRecord {
    pub text: String,
    pub status: ClaimStatus,
    #[serde(default)]
    pub verify_command: Option<String>,
    #[serde(default)]
    pub verify_exit: Option<i32>,
    #[serde(default)]
    pub verify_output_digest: Option<String>,
    /// Why the claim is not verified, when it is not.
    #[serde(default)]
    pub reason: Option<String>,
    /// The proof run on the work with its changes undone, for a step that
    /// declares `fails_before`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Before>,
}

/// The proof as it ran on a copy of the work with every change since a
/// revision undone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Before {
    /// The commit the changes were undone to.
    pub commit: String,
    pub exit_code: Option<i32>,
    /// Digest of the proof command's combined output there.
    pub output_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDigest {
    pub path: String,
    pub digest: String,
    /// The file was executable. Content alone does not make a script
    /// runnable, so the mode is part of what was recorded and restored.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepRef {
    pub step_id: String,
    pub receipt_id: String,
}

/// Which flot derived the receipt. A different deriver may compute identity
/// differently, so it belongs inside the hashed body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolIdentity {
    pub name: String,
    pub version: String,
    pub schema: u32,
}

impl Default for ToolIdentity {
    fn default() -> Self {
        ToolIdentity {
            name: "flot".into(),
            version: FLOT_VERSION.into(),
            schema: SCHEMA_VERSION,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptIdentity {
    pub flow: String,
    pub step_id: String,
    /// Digest of everything that should force a re-run when it changes.
    pub step_key: String,
    pub command: String,
    pub status: StepStatus,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub stdout_digest: String,
    pub stderr_digest: String,
    pub inputs: Vec<FileDigest>,
    /// Content of the files the proof read, as they were when the step began.
    pub verify_inputs: Vec<FileDigest>,
    /// Files the step produced, each kept in the object store, so a reused
    /// receipt can put back exactly what its claims were proven against.
    pub outputs: Vec<FileDigest>,
    /// Every other file the step created, edited or deleted that flot saw it
    /// change, with the digest of its content as the step left it, or `None`
    /// for one it deleted. Its claims were proven on those files, so the
    /// receipt stands in for the step only while each one is still so.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub changed: BTreeMap<String, Option<String>>,
    pub deps: Vec<DepRef>,
    pub claims: Vec<ClaimRecord>,
    /// Known gaps in this receipt's evidence, stated rather than implied.
    pub limitations: Vec<String>,
    pub tool: ToolIdentity,
    /// The candidate of the step that did this work, when the step names
    /// candidates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
    /// The confinement the step ran in, when its flow asked for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxRecord>,
}

/// What a step asked to be confined to, and how that was enforced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxRecord {
    pub network: bool,
    /// Paths relative to the flow root the step could write beneath; `None`
    /// when writes were not restricted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<Vec<String>>,
    /// How the policy was enforced, such as `landlock abi 7, network namespace`.
    pub mechanism: String,
}

impl Observed {
    /// Whether the inline previews and byte counts describe these complete
    /// streams. Previews are what a reader sees first, so a preview that does
    /// not come from the stored output is a forgery, whatever signs it.
    /// A cut preview is a beginning of the stream, or a beginning, the
    /// [`OMITTED`] marker and an end of it: nothing in it was not printed.
    pub fn describes(&self, stdout: &[u8], stderr: &[u8]) -> bool {
        fn preview_of(inline: &str, truncated: bool, bytes: usize, data: &[u8]) -> bool {
            let whole = String::from_utf8_lossy(data);
            let cut = |head: &str, tail: &str| {
                head.len() + tail.len() < whole.len()
                    && whole.starts_with(head)
                    && whole.ends_with(tail)
            };
            bytes == data.len()
                && if truncated {
                    cut(inline, "")
                        || inline
                            .match_indices(OMITTED)
                            .any(|(at, _)| cut(&inline[..at], &inline[at + OMITTED.len()..]))
                } else {
                    inline == whole
                }
        }
        preview_of(
            &self.stdout_inline,
            self.stdout_truncated,
            self.stdout_bytes,
            stdout,
        ) && preview_of(
            &self.stderr_inline,
            self.stderr_truncated,
            self.stderr_bytes,
            stderr,
        )
    }
}

/// Stands where a stream preview leaves out the middle of the output.
pub const OMITTED: &str = "\n[... output omitted here; the complete stream is in the store ...]\n";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Observed {
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub host_os: String,
    /// Bounded preview of stdout. The complete stream stays in the object store
    /// under `stdout_digest`.
    pub stdout_inline: String,
    pub stdout_truncated: bool,
    pub stderr_inline: String,
    pub stderr_truncated: bool,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    /// What the step reported through `$FLOT_METRICS`; `None` when it reported
    /// nothing or did not execute.
    #[serde(default)]
    pub metrics: Option<Metrics>,
    /// Problems with the metrics file. Kept out of the hashed body, because
    /// what a step bills is an observation, not part of the work.
    #[serde(default)]
    pub metrics_notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub receipt_id: String,
    pub identity: ReceiptIdentity,
    pub observed: Observed,
}

impl Receipt {
    pub fn seal(identity: ReceiptIdentity, observed: Observed) -> Self {
        Receipt {
            receipt_id: digest::canonical(&identity),
            identity,
            observed,
        }
    }

    /// Digest of the observation, as a manifest records it.
    pub fn observed_digest(&self) -> String {
        digest::canonical(&self.observed)
    }

    /// Recompute the receipt id from the hashed body.
    pub fn recomputed_id(&self) -> String {
        digest::canonical(&self.identity)
    }

    pub fn is_authentic(&self) -> bool {
        self.recomputed_id() == self.receipt_id
    }

    /// Whether a later run may reuse this receipt instead of executing again.
    ///
    /// Only settled outcomes qualify. A failure, timeout or cancellation might
    /// not repeat, and neither might a refuted or timed-out proof, so those are
    /// always retried; so is a failure `allow_failure` tolerated, which is `ok`
    /// without exiting zero. A claim with no verify command stays unverified
    /// however often it runs, so it does not block reuse.
    pub fn is_reusable(&self) -> bool {
        self.identity.status.is_ok()
            && self.identity.exit_code == Some(0)
            && self.identity.claims.iter().all(|claim| match claim.status {
                ClaimStatus::Verified => true,
                ClaimStatus::Unverified => claim.verify_command.is_none(),
                ClaimStatus::Refuted => false,
            })
    }

    /// Every store object this receipt references: output streams, produced
    /// files and proof output. A receipt is only whole while all of them exist,
    /// and they are what travels with it in a bundle.
    pub fn objects(&self) -> Vec<&str> {
        let mut objects = vec![
            self.identity.stdout_digest.as_str(),
            self.identity.stderr_digest.as_str(),
        ];
        objects.extend(
            self.identity
                .outputs
                .iter()
                .map(|file| file.digest.as_str()),
        );
        for claim in &self.identity.claims {
            objects.extend(claim.verify_output_digest.as_deref());
            objects.extend(
                claim
                    .before
                    .as_ref()
                    .map(|before| before.output_digest.as_str()),
            );
        }
        objects
    }

    pub fn claim_counts(&self) -> ClaimCounts {
        let mut counts = ClaimCounts::default();
        for claim in &self.identity.claims {
            match claim.status {
                ClaimStatus::Verified => counts.verified += 1,
                ClaimStatus::Unverified => counts.unverified += 1,
                ClaimStatus::Refuted => counts.refuted += 1,
            }
        }
        counts
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimCounts {
    pub verified: usize,
    pub unverified: usize,
    pub refuted: usize,
}

impl ClaimCounts {
    pub fn total(&self) -> usize {
        self.verified + self.unverified + self.refuted
    }

    pub fn add(&mut self, other: ClaimCounts) {
        self.verified += other.verified;
        self.unverified += other.unverified;
        self.refuted += other.refuted;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every step reached `Ok` and every declared claim is verified.
    Passed,
    /// Work completed but at least one claim has no proof behind it.
    Unverified,
    /// A step failed, timed out, or a claim was refuted.
    Failed,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Passed => "passed",
            Verdict::Unverified => "unverified",
            Verdict::Failed => "failed",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Verdict::Passed => 0,
            Verdict::Unverified => 2,
            Verdict::Failed => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStep {
    pub step_id: String,
    pub receipt_id: String,
    pub status: StepStatus,
    /// True when the receipt was reused from a previous run instead of re-run.
    pub cached: bool,
    pub claims: ClaimCounts,
    /// What this run observed of the step: output previews, timing and
    /// reported spend, stored as an object named by this digest. The receipt
    /// id covers the work, not one execution of it, so each run keeps its own
    /// observation, and a signature over the manifest covers it.
    pub observed_digest: String,
    /// Earlier attempts this run made at the step, oldest first, each of
    /// which did not hold. The step's receipt is the last attempt's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<RunAttempt>,
    /// The step's other candidates this run executed, when it names
    /// candidates. The step's receipt is the kept candidate's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<RunCandidate>,
    /// Signer keys of the imported run the reused receipt came from: whose
    /// word stands in for the work here. Empty when this host executed the
    /// step, or reused a receipt it executed itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vouched_by: Vec<String>,
}

/// An attempt at a step that did not hold, kept as evidence of what it took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunAttempt {
    pub receipt_id: String,
    pub observed_digest: String,
}

/// A candidate of a step that this run executed and did not keep: its last
/// receipt, that receipt's observation, and its earlier attempts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCandidate {
    pub name: String,
    pub receipt_id: String,
    pub observed_digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<RunAttempt>,
}

/// How a step's candidates are ranked, best first: one whose last receipt
/// held, and among those lower reported cost over all its attempts, an
/// unknown cost after every known one. Candidates that did not hold rank
/// equal. Equal ranks keep the earlier candidate.
pub fn candidate_rank(last: &Receipt, cost_micro_usd: Option<u64>) -> (bool, bool, u64) {
    if !last.is_reusable() {
        return (true, false, 0);
    }
    (false, cost_micro_usd.is_none(), cost_micro_usd.unwrap_or(0))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunManifest {
    pub run_id: String,
    pub flow: String,
    pub flow_digest: String,
    /// Fold of the ordered step receipt ids. Deterministic flows reproduce it.
    pub root_digest: String,
    pub verdict: Verdict,
    /// Set when the run selected only some of the flow's steps: its verdict
    /// speaks for those, not for the flow. Absent from a run of every step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<Partial>,
    pub steps: Vec<RunStep>,
    pub claims: ClaimCounts,
    pub limitations: Vec<String>,
    /// How many steps this run was allowed to overlap.
    pub max_parallel: usize,
    /// Tokens and cost reported by steps, split into spent and reused.
    pub spend: Spend,
    /// Run cost ceiling in micro-dollars, when one was set.
    pub max_cost_micro_usd: Option<u64>,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub flot_version: String,
    /// Edits and commands the guard refused after the run before this one
    /// executed here finished, up to the end of this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refusals: Vec<Refusal>,
}

/// How many of a flow's steps a run selected, when not all of them: the step
/// asked for and every step it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partial {
    pub selected: usize,
    /// The steps the flow has.
    pub of: usize,
}

/// An edit or a shell command the guard refused an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub at_ms: u64,
    /// The agent's tool, such as `Edit` or `Bash`.
    pub tool: String,
    /// What it would have touched, and why that is refused.
    pub reasons: Vec<String>,
}

impl RunManifest {
    pub fn root_of(step_receipts: &[(String, String)]) -> String {
        let members: Vec<String> = step_receipts
            .iter()
            .map(|(step_id, receipt_id)| format!("{step_id}@{receipt_id}"))
            .collect();
        digest::fold("flot.run", &members)
    }

    pub fn step(&self, step_id: &str) -> Option<&RunStep> {
        self.steps.iter().find(|step| step.step_id == step_id)
    }

    /// The id a run with this start time and root must carry. Run ids name
    /// files in a store, so one that is not exactly this is never accepted.
    pub fn id_for(started_at_ms: u64, root_digest: &str) -> String {
        format!("{:013}-{}", started_at_ms, digest::short(root_digest))
    }

    /// Everything in this manifest that does not follow from the receipts it
    /// names. Its run id, step list, per-step status and claim counts, totals,
    /// verdict and root are all derived facts, so each is recomputed rather
    /// than believed; a manifest is only as good as its receipts. Each receipt
    /// must also be authentic, belong to this flow and step, point at the
    /// dependency receipts this run recorded, and have its limitations listed
    /// on the manifest. Import and audit both judge a run by this one rule. `receipts`
    /// maps receipt id to receipt and `observations` maps observation digest to
    /// observation; a step whose receipt or observation is absent is reported.
    pub fn disagreements(
        &self,
        receipts: &BTreeMap<&str, &Receipt>,
        observations: &BTreeMap<&str, Observed>,
    ) -> Vec<String> {
        let mut found = Vec::new();
        if self.run_id != Self::id_for(self.started_at_ms, &self.root_digest) {
            found.push(format!(
                "run id `{}` is not derived from its start time and root",
                self.run_id
            ));
        }
        // Whether a run selected every step cannot be read off its receipts;
        // a signature covers that. What can be checked is that a selection
        // lists the steps it names and leaves some out.
        if let Some(partial) = self.partial {
            if partial.selected >= partial.of {
                found.push(format!(
                    "partial run of {} of {} steps leaves no step out",
                    partial.selected, partial.of
                ));
            }
            if partial.selected != self.steps.len() {
                found.push(format!(
                    "partial run of {} of {} steps lists {} step(s)",
                    partial.selected,
                    partial.of,
                    self.steps.len()
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut statuses = Vec::new();
        let mut totals = ClaimCounts::default();
        let mut spend = Spend::default();
        for step in &self.steps {
            if !crate::core::flow::is_valid_step_id(&step.step_id) {
                found.push(format!("step id `{}` is not a valid step id", step.step_id));
            }
            if !seen.insert(step.step_id.as_str()) {
                found.push(format!("step `{}` appears more than once", step.step_id));
            }
            // Only a reused receipt rests on someone else's word, and a
            // signer is named once, spelled as flot spells keys, so a report
            // can match it against attestations and the trust list.
            if !step.vouched_by.is_empty() && !step.cached {
                found.push(format!(
                    "{}: names signers who vouched for it, but the step was not reused",
                    step.step_id
                ));
            }
            let mut signers = std::collections::BTreeSet::new();
            for key in &step.vouched_by {
                if crate::core::attest::canonical_key(key).ok().as_deref() != Some(key.as_str()) {
                    found.push(format!(
                        "{}: `{}` is not a signer key as flot writes one",
                        step.step_id,
                        crate::core::attest::fingerprint(key)
                    ));
                }
                if !signers.insert(key.as_str()) {
                    found.push(format!(
                        "{}: signer {} is named more than once",
                        step.step_id,
                        crate::core::attest::fingerprint(key)
                    ));
                }
            }
            let Some(receipt) = receipts.get(step.receipt_id.as_str()) else {
                found.push(format!(
                    "{}: receipt {} is not available",
                    step.step_id,
                    digest::short(&step.receipt_id)
                ));
                continue;
            };
            self.belongs(&step.step_id, receipt, &mut found);
            if step.cached && !(step.attempts.is_empty() && step.candidates.is_empty()) {
                found.push(format!(
                    "{}: a reused receipt lists attempts this run made",
                    step.step_id
                ));
            }
            let kept_cost = self.attempts_judged(
                &step.step_id,
                &step.step_id,
                &step.attempts,
                receipts,
                observations,
                &mut spend,
                &mut found,
            );
            let kept_cost = sum_costs(
                kept_cost,
                observations
                    .get(step.observed_digest.as_str())
                    .and_then(|observed| observed.metrics)
                    .and_then(|metrics| metrics.cost_micro_usd),
            );
            if !step.candidates.is_empty() && receipt.identity.candidate.is_none() {
                found.push(format!(
                    "{}: the run lists other candidates but the receipt names none",
                    step.step_id
                ));
            }
            let mut names = std::collections::BTreeSet::new();
            names.extend(receipt.identity.candidate.as_deref());
            for candidate in &step.candidates {
                let label = format!("{} candidate {}", step.step_id, candidate.name);
                if !names.insert(candidate.name.as_str()) {
                    found.push(format!("{label}: the candidate appears more than once"));
                }
                let earlier_cost = self.attempts_judged(
                    &step.step_id,
                    &label,
                    &candidate.attempts,
                    receipts,
                    observations,
                    &mut spend,
                    &mut found,
                );
                let Some(last) = receipts.get(candidate.receipt_id.as_str()) else {
                    found.push(format!(
                        "{label}: receipt {} is not available",
                        digest::short(&candidate.receipt_id)
                    ));
                    continue;
                };
                self.belongs(&label, last, &mut found);
                if last.identity.step_id != step.step_id {
                    found.push(format!(
                        "{label}: receipt belongs to step `{}`",
                        last.identity.step_id
                    ));
                }
                if last.identity.candidate.as_deref() != Some(candidate.name.as_str()) {
                    found.push(format!("{label}: receipt is not that candidate's"));
                }
                let Some(observed) = observations.get(candidate.observed_digest.as_str()) else {
                    found.push(format!(
                        "{label}: observation {} is not available",
                        digest::short(&candidate.observed_digest)
                    ));
                    continue;
                };
                spend.add_executed(observed.metrics);
                let cost = sum_costs(
                    earlier_cost,
                    observed.metrics.and_then(|metrics| metrics.cost_micro_usd),
                );
                // The kept candidate must rank at least as well as each
                // other one, by the rule that chose it.
                if candidate_rank(last, cost) < candidate_rank(receipt, kept_cost) {
                    found.push(format!(
                        "{label}: it ranks above the candidate the run kept"
                    ));
                }
            }
            if receipt.identity.step_id != step.step_id {
                found.push(format!(
                    "{}: receipt belongs to step `{}`",
                    step.step_id, receipt.identity.step_id
                ));
            }
            // What the receipt says went wrong must be on the manifest: a
            // manifest that drops a step's limitation reads cleaner than the
            // work was.
            for limitation in &receipt.identity.limitations {
                let listed = format!("{}: {}", step.step_id, limitation);
                if !self.limitations.contains(&listed) {
                    found.push(format!(
                        "{}: manifest omits the receipt's limitation `{}`",
                        step.step_id, limitation
                    ));
                }
            }
            if receipt.identity.status != step.status {
                found.push(format!(
                    "{}: manifest says {} but the receipt says {}",
                    step.step_id,
                    step.status.label(),
                    receipt.identity.status.label()
                ));
            }
            let Some(observed) = observations.get(step.observed_digest.as_str()) else {
                found.push(format!(
                    "{}: observation {} is not available",
                    step.step_id,
                    digest::short(&step.observed_digest)
                ));
                continue;
            };
            if step.cached {
                spend.add_reused(observed.metrics);
            } else if receipt.identity.status != StepStatus::Cancelled {
                spend.add_executed(observed.metrics);
            }
            let counts = receipt.claim_counts();
            if counts != step.claims {
                found.push(format!(
                    "{}: manifest claim counts disagree with the receipt",
                    step.step_id
                ));
            }
            totals.add(counts);
            statuses.push(RunStep {
                status: receipt.identity.status,
                claims: counts,
                ..step.clone()
            });
        }
        if found.is_empty() {
            if totals != self.claims {
                found.push("run claim totals disagree with its receipts".into());
            }
            if spend != self.spend {
                found.push("run spend disagrees with what its receipts reported".into());
            }
            let verdict = verdict_for(&statuses, totals);
            if verdict != self.verdict {
                found.push(format!(
                    "manifest verdict is {} but its receipts make it {}",
                    self.verdict.label(),
                    verdict.label()
                ));
            }
        }
        let pairs: Vec<(String, String)> = self
            .steps
            .iter()
            .map(|step| (step.step_id.clone(), step.receipt_id.clone()))
            .collect();
        if Self::root_of(&pairs) != self.root_digest {
            found.push("run root digest does not match its steps".into());
        }
        found
    }

    /// Judge a list of earlier attempts at a step and add what they spent.
    /// Returns their total reported cost, `None` when any of them is unknown.
    #[allow(clippy::too_many_arguments)]
    fn attempts_judged(
        &self,
        step_id: &str,
        label: &str,
        attempts: &[RunAttempt],
        receipts: &BTreeMap<&str, &Receipt>,
        observations: &BTreeMap<&str, Observed>,
        spend: &mut Spend,
        found: &mut Vec<String>,
    ) -> Option<u64> {
        let mut cost = Some(0);
        for (number, attempt) in attempts.iter().enumerate() {
            let label = format!("{label} attempt {}", number + 1);
            let Some(earlier) = receipts.get(attempt.receipt_id.as_str()) else {
                found.push(format!(
                    "{label}: receipt {} is not available",
                    digest::short(&attempt.receipt_id)
                ));
                continue;
            };
            self.belongs(&label, earlier, found);
            if earlier.identity.step_id != step_id {
                found.push(format!(
                    "{label}: receipt belongs to step `{}`",
                    earlier.identity.step_id
                ));
            }
            // The step is only tried again when an attempt did not hold.
            if earlier.is_reusable() {
                found.push(format!(
                    "{label}: it held, so the step would not have been tried again"
                ));
            }
            match observations.get(attempt.observed_digest.as_str()) {
                Some(observed) => {
                    spend.add_executed(observed.metrics);
                    cost = sum_costs(cost, observed.metrics.and_then(|m| m.cost_micro_usd));
                }
                None => {
                    cost = None;
                    found.push(format!(
                        "{label}: observation {} is not available",
                        digest::short(&attempt.observed_digest)
                    ))
                }
            }
        }
        cost
    }

    /// What every receipt a run names must be: authentic, of this flow, with
    /// proof behind each verified claim, built on the receipts this run
    /// recorded for its dependencies.
    fn belongs(&self, label: &str, receipt: &Receipt, found: &mut Vec<String>) {
        if !receipt.is_authentic() {
            found.push(format!(
                "{label}: receipt body does not match its id {}",
                digest::short(&receipt.receipt_id)
            ));
        }
        for dependency in &receipt.identity.deps {
            match self.step(&dependency.step_id) {
                Some(recorded) if recorded.receipt_id == dependency.receipt_id => {}
                Some(_) => found.push(format!(
                    "{label}: references a different receipt for `{}` than the run recorded",
                    dependency.step_id
                )),
                None => found.push(format!(
                    "{label}: references `{}`, which is not in this run",
                    dependency.step_id
                )),
            }
        }
        if receipt.identity.claims.iter().any(|claim| {
            claim.status == ClaimStatus::Verified && claim.verify_output_digest.is_none()
        }) {
            found.push(format!(
                "{label}: claim is marked verified with no proof object"
            ));
        }
        if receipt.identity.flow != self.flow {
            found.push(format!(
                "{label}: receipt belongs to flow `{}`",
                receipt.identity.flow
            ));
        }
    }

    /// Every receipt this run names with the observation it records for it:
    /// each step's, and each earlier attempt's.
    pub fn evidence(&self) -> Vec<(&str, &str, &str)> {
        let mut named = Vec::new();
        for step in &self.steps {
            for candidate in &step.candidates {
                for attempt in &candidate.attempts {
                    named.push((
                        step.step_id.as_str(),
                        attempt.receipt_id.as_str(),
                        attempt.observed_digest.as_str(),
                    ));
                }
                named.push((
                    step.step_id.as_str(),
                    candidate.receipt_id.as_str(),
                    candidate.observed_digest.as_str(),
                ));
            }
            for attempt in &step.attempts {
                named.push((
                    step.step_id.as_str(),
                    attempt.receipt_id.as_str(),
                    attempt.observed_digest.as_str(),
                ));
            }
            named.push((
                step.step_id.as_str(),
                step.receipt_id.as_str(),
                step.observed_digest.as_str(),
            ));
        }
        named
    }
}

/// Two costs together; unknown when either is, since an execution that
/// reported no cost may have cost anything.
pub fn sum_costs(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    Some(a?.saturating_add(b?))
}

/// The verdict rule, in one place: failures dominate, then refuted claims, then
/// unproven claims. Success requires proof, not absence of failure.
pub fn verdict_for(steps: &[RunStep], claims: ClaimCounts) -> Verdict {
    if steps.iter().any(|step| !step.status.is_ok()) || claims.refuted > 0 {
        return Verdict::Failed;
    }
    if claims.unverified > 0 {
        return Verdict::Unverified;
    }
    Verdict::Passed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(step: &str, stdout: &str) -> ReceiptIdentity {
        ReceiptIdentity {
            flow: "demo".into(),
            step_id: step.into(),
            step_key: "sha256:key".into(),
            command: "echo hi".into(),
            status: StepStatus::Ok,
            exit_code: Some(0),
            stdout_digest: digest::bytes(stdout.as_bytes()),
            stderr_digest: digest::bytes(b""),
            inputs: vec![],
            verify_inputs: vec![],
            outputs: vec![],
            changed: BTreeMap::new(),
            deps: vec![],
            claims: vec![],
            limitations: vec![],
            tool: ToolIdentity::default(),
            candidate: None,
            sandbox: None,
        }
    }

    fn run_step(step: &str, status: StepStatus, claims: ClaimCounts) -> RunStep {
        RunStep {
            observed_digest: String::new(),
            attempts: Vec::new(),
            candidates: Vec::new(),
            step_id: step.into(),
            receipt_id: "sha256:x".into(),
            status,
            cached: false,
            claims,
            vouched_by: Vec::new(),
        }
    }

    #[test]
    fn objects_cover_every_piece_of_evidence() {
        let proof = digest::bytes(b"proof");
        let mut body = identity("a", "hello");
        body.outputs.push(FileDigest {
            executable: false,
            path: "out.txt".into(),
            digest: digest::bytes(b"out"),
        });
        let before = digest::bytes(b"before");
        body.claims.push(ClaimRecord {
            verify_output_digest: Some(proof.clone()),
            before: Some(Before {
                commit: "abc".into(),
                exit_code: Some(1),
                output_digest: before.clone(),
            }),
            ..claim(ClaimStatus::Verified, Some("true"))
        });
        let receipt = Receipt::seal(body, Observed::default());
        let objects = receipt.objects();
        assert!(objects.contains(&receipt.identity.stdout_digest.as_str()));
        assert!(objects.contains(&receipt.identity.stderr_digest.as_str()));
        assert!(objects.contains(&digest::bytes(b"out").as_str()));
        assert!(objects.contains(&proof.as_str()));
        assert!(objects.contains(&before.as_str()));
    }

    #[test]
    fn receipt_id_ignores_timing_but_tracks_output() {
        let fast = Receipt::seal(
            identity("a", "hello"),
            Observed {
                duration_ms: 1,
                started_at_ms: 1000,
                ..Observed::default()
            },
        );
        let slow = Receipt::seal(
            identity("a", "hello"),
            Observed {
                duration_ms: 9999,
                started_at_ms: 777_777,
                ..Observed::default()
            },
        );
        assert_eq!(fast.receipt_id, slow.receipt_id);

        let other_output = Receipt::seal(identity("a", "goodbye"), Observed::default());
        assert_ne!(fast.receipt_id, other_output.receipt_id);
    }

    #[test]
    fn tampering_with_the_hashed_body_is_detectable() {
        let mut receipt = Receipt::seal(identity("a", "hello"), Observed::default());
        assert!(receipt.is_authentic());
        receipt.identity.exit_code = Some(0);
        receipt.identity.status = StepStatus::Failed;
        assert!(!receipt.is_authentic());
    }

    #[test]
    fn editing_only_observation_keeps_the_receipt_authentic() {
        let mut receipt = Receipt::seal(identity("a", "hello"), Observed::default());
        receipt.observed.duration_ms = 42;
        assert!(receipt.is_authentic());
    }

    fn claim(status: ClaimStatus, verify_command: Option<&str>) -> ClaimRecord {
        ClaimRecord {
            text: "holds".into(),
            status,
            verify_command: verify_command.map(str::to_string),
            verify_exit: None,
            verify_output_digest: None,
            reason: None,
            before: None,
        }
    }

    fn with(status: StepStatus, claims: Vec<ClaimRecord>) -> Receipt {
        let mut body = identity("a", "out");
        body.status = status;
        body.claims = claims;
        Receipt::seal(body, Observed::default())
    }

    #[test]
    fn only_settled_success_is_reusable() {
        assert!(with(StepStatus::Ok, vec![]).is_reusable());
        assert!(with(
            StepStatus::Ok,
            vec![claim(ClaimStatus::Verified, Some("true"))]
        )
        .is_reusable());
        for status in [
            StepStatus::Failed,
            StepStatus::Timeout,
            StepStatus::Cancelled,
        ] {
            assert!(!with(status, vec![]).is_reusable(), "{status:?}");
        }
        // `ok` under allow_failure without exiting zero is a failure kept
        // from failing the run, and is tried again like one.
        let mut tolerated = identity("a", "out");
        tolerated.exit_code = Some(1);
        assert!(!Receipt::seal(tolerated, Observed::default()).is_reusable());
    }

    #[test]
    fn a_receipt_that_changed_nothing_has_the_bytes_it_always_had() {
        let quiet = Receipt::seal(identity("a", "out"), Observed::default());
        assert!(!serde_json::to_string(&quiet).unwrap().contains("changed"));
        let mut body = identity("a", "out");
        body.changed
            .insert("src/lib.txt".into(), Some(digest::bytes(b"new")));
        assert_ne!(
            Receipt::seal(body, Observed::default()).receipt_id,
            quiet.receipt_id
        );
    }

    /// A receipt with every optional part set, named after a release that
    /// never happens so that releases leave the goldens below as they are.
    fn golden_receipt() -> Receipt {
        let identity = ReceiptIdentity {
            flow: "golden".into(),
            step_id: "build".into(),
            step_key: digest::bytes(b"step key"),
            command: "make out.txt".into(),
            status: StepStatus::Ok,
            exit_code: Some(0),
            stdout_digest: digest::bytes(b"out\n"),
            stderr_digest: digest::bytes(b""),
            inputs: vec![
                FileDigest {
                    path: "run.sh".into(),
                    digest: digest::bytes(b"make\n"),
                    executable: true,
                },
                FileDigest {
                    path: "src/a.txt".into(),
                    digest: digest::bytes(b"a"),
                    executable: false,
                },
            ],
            verify_inputs: vec![FileDigest {
                path: "check.sh".into(),
                digest: digest::bytes(b"grep -q 1 out.txt\n"),
                executable: false,
            }],
            outputs: vec![FileDigest {
                path: "out.txt".into(),
                digest: digest::bytes(b"1\n"),
                executable: false,
            }],
            changed: BTreeMap::from([
                ("notes.md".into(), Some(digest::bytes(b"notes\n"))),
                ("old.txt".into(), None),
            ]),
            deps: vec![DepRef {
                step_id: "fetch".into(),
                receipt_id: digest::bytes(b"receipt of fetch"),
            }],
            claims: vec![
                ClaimRecord {
                    text: "out.txt holds 1".into(),
                    status: ClaimStatus::Verified,
                    verify_command: Some("sh check.sh".into()),
                    verify_exit: Some(0),
                    verify_output_digest: Some(digest::bytes(b"")),
                    reason: None,
                    before: Some(Before {
                        commit: "0123456789abcdef0123456789abcdef01234567".into(),
                        exit_code: Some(1),
                        output_digest: digest::bytes(b"no\n"),
                    }),
                },
                ClaimRecord {
                    text: "the docs say so".into(),
                    status: ClaimStatus::Unverified,
                    verify_command: None,
                    verify_exit: None,
                    verify_output_digest: None,
                    reason: Some("no verify command".into()),
                    before: None,
                },
            ],
            limitations: vec!["step declares no inputs".into()],
            tool: ToolIdentity {
                name: "flot".into(),
                version: "0.0.0-golden".into(),
                schema: SCHEMA_VERSION,
            },
            candidate: Some("fast".into()),
            sandbox: Some(SandboxRecord {
                network: false,
                write: Some(vec!["out".into()]),
                mechanism: "landlock abi 7, network namespace".into(),
            }),
        };
        let observed = Observed {
            started_at_ms: 1_700_000_000_000,
            duration_ms: 1234,
            host_os: "linux".into(),
            stdout_inline: "out\n".into(),
            stdout_truncated: false,
            stderr_inline: String::new(),
            stderr_truncated: false,
            stdout_bytes: 4,
            stderr_bytes: 0,
            metrics: Some(Metrics {
                input_tokens: 10,
                output_tokens: 20,
                cost_micro_usd: Some(300),
            }),
            metrics_notes: vec!["cost was rounded".into()],
        };
        Receipt::seal(identity, observed)
    }

    /// The exact bytes a receipt is named by, stored as and observed as.
    /// Field order is byte order, so these change only with `SCHEMA_VERSION`
    /// or the store format.
    #[test]
    fn a_receipt_has_exactly_these_bytes() {
        let receipt = golden_receipt();
        assert_eq!(
            digest::canonical_json(&receipt.identity),
            GOLDEN_IDENTITY,
            "the body the receipt id digests"
        );
        assert_eq!(receipt.receipt_id, GOLDEN_RECEIPT_ID);
        assert_eq!(
            serde_json::to_string_pretty(&receipt).unwrap(),
            GOLDEN_RECEIPT,
            "the text the store keeps"
        );
        assert_eq!(
            digest::canonical_json(&receipt.observed),
            GOLDEN_OBSERVED,
            "the observation a run stores"
        );
        assert_eq!(receipt.observed_digest(), GOLDEN_OBSERVED_DIGEST);

        // Unset optional parts leave no trace in the body.
        let mut quiet = receipt.identity.clone();
        quiet.changed.clear();
        quiet.claims.clear();
        quiet.inputs.clear();
        quiet.candidate = None;
        quiet.sandbox = None;
        let quiet = Receipt::seal(quiet, Observed::default());
        assert_eq!(
            digest::canonical_json(&quiet.identity),
            GOLDEN_QUIET_IDENTITY
        );
        assert_eq!(quiet.receipt_id, GOLDEN_QUIET_RECEIPT_ID);
    }

    const GOLDEN_IDENTITY: &str = r#"{"candidate":"fast","changed":{"notes.md":"sha256:444e0fffbd825e9610ff5b199485707a0c895339ae80c15cc8a8aee41b106fda","old.txt":null},"claims":[{"before":{"commit":"0123456789abcdef0123456789abcdef01234567","exit_code":1,"output_digest":"sha256:564739ea8fa5926d4fa5c9734fed462061960a22e6b8d5c06e94969d97891bf2"},"reason":null,"status":"verified","text":"out.txt holds 1","verify_command":"sh check.sh","verify_exit":0,"verify_output_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"},{"reason":"no verify command","status":"unverified","text":"the docs say so","verify_command":null,"verify_exit":null,"verify_output_digest":null}],"command":"make out.txt","deps":[{"receipt_id":"sha256:ddb195b83f23632aecf220803ccfb3b94b79146dbca28b2a7014df8e86766861","step_id":"fetch"}],"exit_code":0,"flow":"golden","inputs":[{"digest":"sha256:c117ade9ea8a00abe926dfed79297a9a3632defb97a3ffd1baf33fe92704aa0f","executable":true,"path":"run.sh"},{"digest":"sha256:ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb","path":"src/a.txt"}],"limitations":["step declares no inputs"],"outputs":[{"digest":"sha256:4355a46b19d348dc2f57c046f8ef63d4538ebb936000f3c9ee954a27460dd865","path":"out.txt"}],"sandbox":{"mechanism":"landlock abi 7, network namespace","network":false,"write":["out"]},"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:54034ac5c6e9ea95734ec2b729fd6d62abf64af34a9f9ce5d466cb788191a73d","step_id":"build","step_key":"sha256:9bcae35dd64134b0c70ba26408a1b8f9f90d80123bdc5462d44d203de3d939f7","tool":{"name":"flot","schema":5,"version":"0.0.0-golden"},"verify_inputs":[{"digest":"sha256:a8b1f082c2590c4c3855b71427328ecab1ebb40701108a07be7b70f9c926f225","path":"check.sh"}]}"#;

    const GOLDEN_RECEIPT_ID: &str =
        r#"sha256:5c7388eef6d66bbab54cfa95dd33f80b29975e31a7c2a8f9ffcca475e1f9f232"#;

    const GOLDEN_RECEIPT: &str = r#"{
  "receipt_id": "sha256:5c7388eef6d66bbab54cfa95dd33f80b29975e31a7c2a8f9ffcca475e1f9f232",
  "identity": {
    "flow": "golden",
    "step_id": "build",
    "step_key": "sha256:9bcae35dd64134b0c70ba26408a1b8f9f90d80123bdc5462d44d203de3d939f7",
    "command": "make out.txt",
    "status": "ok",
    "exit_code": 0,
    "stdout_digest": "sha256:54034ac5c6e9ea95734ec2b729fd6d62abf64af34a9f9ce5d466cb788191a73d",
    "stderr_digest": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "inputs": [
      {
        "path": "run.sh",
        "digest": "sha256:c117ade9ea8a00abe926dfed79297a9a3632defb97a3ffd1baf33fe92704aa0f",
        "executable": true
      },
      {
        "path": "src/a.txt",
        "digest": "sha256:ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"
      }
    ],
    "verify_inputs": [
      {
        "path": "check.sh",
        "digest": "sha256:a8b1f082c2590c4c3855b71427328ecab1ebb40701108a07be7b70f9c926f225"
      }
    ],
    "outputs": [
      {
        "path": "out.txt",
        "digest": "sha256:4355a46b19d348dc2f57c046f8ef63d4538ebb936000f3c9ee954a27460dd865"
      }
    ],
    "changed": {
      "notes.md": "sha256:444e0fffbd825e9610ff5b199485707a0c895339ae80c15cc8a8aee41b106fda",
      "old.txt": null
    },
    "deps": [
      {
        "step_id": "fetch",
        "receipt_id": "sha256:ddb195b83f23632aecf220803ccfb3b94b79146dbca28b2a7014df8e86766861"
      }
    ],
    "claims": [
      {
        "text": "out.txt holds 1",
        "status": "verified",
        "verify_command": "sh check.sh",
        "verify_exit": 0,
        "verify_output_digest": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "reason": null,
        "before": {
          "commit": "0123456789abcdef0123456789abcdef01234567",
          "exit_code": 1,
          "output_digest": "sha256:564739ea8fa5926d4fa5c9734fed462061960a22e6b8d5c06e94969d97891bf2"
        }
      },
      {
        "text": "the docs say so",
        "status": "unverified",
        "verify_command": null,
        "verify_exit": null,
        "verify_output_digest": null,
        "reason": "no verify command"
      }
    ],
    "limitations": [
      "step declares no inputs"
    ],
    "tool": {
      "name": "flot",
      "version": "0.0.0-golden",
      "schema": 5
    },
    "candidate": "fast",
    "sandbox": {
      "network": false,
      "write": [
        "out"
      ],
      "mechanism": "landlock abi 7, network namespace"
    }
  },
  "observed": {
    "started_at_ms": 1700000000000,
    "duration_ms": 1234,
    "host_os": "linux",
    "stdout_inline": "out\n",
    "stdout_truncated": false,
    "stderr_inline": "",
    "stderr_truncated": false,
    "stdout_bytes": 4,
    "stderr_bytes": 0,
    "metrics": {
      "input_tokens": 10,
      "output_tokens": 20,
      "cost_micro_usd": 300
    },
    "metrics_notes": [
      "cost was rounded"
    ]
  }
}"#;

    const GOLDEN_OBSERVED: &str = r#"{"duration_ms":1234,"host_os":"linux","metrics":{"cost_micro_usd":300,"input_tokens":10,"output_tokens":20},"metrics_notes":["cost was rounded"],"started_at_ms":1700000000000,"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":4,"stdout_inline":"out\n","stdout_truncated":false}"#;

    const GOLDEN_OBSERVED_DIGEST: &str =
        r#"sha256:afc80763eb9e085082778af793b5e4ceca56ce8e4cac9a82a243b2d110e2a73d"#;

    const GOLDEN_QUIET_IDENTITY: &str = r#"{"claims":[],"command":"make out.txt","deps":[{"receipt_id":"sha256:ddb195b83f23632aecf220803ccfb3b94b79146dbca28b2a7014df8e86766861","step_id":"fetch"}],"exit_code":0,"flow":"golden","inputs":[],"limitations":["step declares no inputs"],"outputs":[{"digest":"sha256:4355a46b19d348dc2f57c046f8ef63d4538ebb936000f3c9ee954a27460dd865","path":"out.txt"}],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:54034ac5c6e9ea95734ec2b729fd6d62abf64af34a9f9ce5d466cb788191a73d","step_id":"build","step_key":"sha256:9bcae35dd64134b0c70ba26408a1b8f9f90d80123bdc5462d44d203de3d939f7","tool":{"name":"flot","schema":5,"version":"0.0.0-golden"},"verify_inputs":[{"digest":"sha256:a8b1f082c2590c4c3855b71427328ecab1ebb40701108a07be7b70f9c926f225","path":"check.sh"}]}"#;

    const GOLDEN_QUIET_RECEIPT_ID: &str =
        r#"sha256:6bf62e8bf4a81c2fcf69813f3c010d1bfd66193a944438aa6bbdfa8f43a7e328"#;

    #[test]
    fn a_refuted_or_unfinished_proof_is_retried() {
        assert!(!with(
            StepStatus::Ok,
            vec![claim(ClaimStatus::Refuted, Some("false"))]
        )
        .is_reusable());
        // Unverified despite a verify command means the proof did not finish.
        assert!(!with(
            StepStatus::Ok,
            vec![claim(ClaimStatus::Unverified, Some("sleep 99"))]
        )
        .is_reusable());
    }

    #[test]
    fn a_claim_without_any_proof_command_does_not_block_reuse() {
        assert!(with(StepStatus::Ok, vec![claim(ClaimStatus::Unverified, None)]).is_reusable());
    }

    #[test]
    fn failure_outranks_unverified_claims() {
        let steps = vec![
            run_step("a", StepStatus::Failed, ClaimCounts::default()),
            run_step(
                "b",
                StepStatus::Ok,
                ClaimCounts {
                    unverified: 1,
                    ..ClaimCounts::default()
                },
            ),
        ];
        let claims = ClaimCounts {
            unverified: 1,
            ..ClaimCounts::default()
        };
        assert_eq!(verdict_for(&steps, claims), Verdict::Failed);
    }

    #[test]
    fn a_refuted_claim_fails_the_run_even_when_every_step_exited_zero() {
        let claims = ClaimCounts {
            refuted: 1,
            verified: 3,
            unverified: 0,
        };
        let steps = vec![run_step("a", StepStatus::Ok, claims)];
        assert_eq!(verdict_for(&steps, claims), Verdict::Failed);
    }

    #[test]
    fn unproven_claims_never_report_passed() {
        let claims = ClaimCounts {
            unverified: 1,
            verified: 1,
            refuted: 0,
        };
        let steps = vec![run_step("a", StepStatus::Ok, claims)];
        assert_eq!(verdict_for(&steps, claims), Verdict::Unverified);
    }

    #[test]
    fn a_claim_free_run_that_succeeds_passes() {
        let steps = vec![run_step("a", StepStatus::Ok, ClaimCounts::default())];
        assert_eq!(verdict_for(&steps, ClaimCounts::default()), Verdict::Passed);
    }

    #[test]
    fn run_root_is_order_sensitive() {
        let a = ("a".to_string(), "sha256:1".to_string());
        let b = ("b".to_string(), "sha256:2".to_string());
        assert_ne!(
            RunManifest::root_of(&[a.clone(), b.clone()]),
            RunManifest::root_of(&[b, a])
        );
    }

    #[test]
    fn verdict_exit_codes_are_distinct() {
        assert_eq!(Verdict::Passed.exit_code(), 0);
        assert_eq!(Verdict::Failed.exit_code(), 1);
        assert_eq!(Verdict::Unverified.exit_code(), 2);
    }

    /// A one-step run whose manifest follows exactly from its receipt.
    fn coherent(receipt: &Receipt) -> RunManifest {
        let observed_digest = digest::bytes(b"observation");
        let steps = vec![RunStep {
            observed_digest,
            step_id: receipt.identity.step_id.clone(),
            receipt_id: receipt.receipt_id.clone(),
            status: receipt.identity.status,
            cached: false,
            claims: receipt.claim_counts(),
            attempts: Vec::new(),
            candidates: Vec::new(),
            vouched_by: Vec::new(),
        }];
        let root_digest =
            RunManifest::root_of(&[(steps[0].step_id.clone(), steps[0].receipt_id.clone())]);
        RunManifest {
            run_id: RunManifest::id_for(1, &root_digest),
            flow: receipt.identity.flow.clone(),
            flow_digest: String::new(),
            root_digest,
            verdict: verdict_for(&steps, receipt.claim_counts()),
            partial: None,
            claims: receipt.claim_counts(),
            limitations: receipt
                .identity
                .limitations
                .iter()
                .map(|limitation| format!("{}: {limitation}", receipt.identity.step_id))
                .collect(),
            steps,
            max_parallel: 1,
            spend: {
                let mut spend = Spend::default();
                spend.add_executed(Observed::default().metrics);
                spend
            },
            max_cost_micro_usd: None,
            started_at_ms: 1,
            duration_ms: 0,
            flot_version: String::new(),
            refusals: Vec::new(),
        }
    }

    fn judge(manifest: &RunManifest, receipt: &Receipt) -> Vec<String> {
        let receipts = BTreeMap::from([(receipt.receipt_id.as_str(), receipt)]);
        let observations = BTreeMap::from([(
            manifest.steps[0].observed_digest.as_str(),
            Observed::default(),
        )]);
        manifest.disagreements(&receipts, &observations)
    }

    #[test]
    fn a_selection_lists_the_steps_it_names_and_a_signature_covers_it() {
        let receipt = Receipt::seal(identity("a", "out"), Observed::default());
        let mut partial = coherent(&receipt);
        partial.partial = Some(Partial { selected: 1, of: 2 });
        assert_eq!(judge(&partial, &receipt), Vec::<String>::new());
        // A run of every step records no selection, so its bytes are as
        // they were before selections were recorded.
        assert!(!serde_json::to_string(&coherent(&receipt))
            .unwrap()
            .contains("partial"));

        for (selected, of, expected) in [
            (1, 1, "partial run of 1 of 1 steps leaves no step out"),
            (2, 3, "partial run of 2 of 3 steps lists 1 step(s)"),
        ] {
            let mut forged = coherent(&receipt);
            forged.partial = Some(Partial { selected, of });
            assert_eq!(judge(&forged, &receipt), vec![expected.to_string()]);
        }

        // Dropping the selection from a signed run breaks the signature.
        let attestation = crate::core::attest::sign(&[7; 32], &partial);
        assert_eq!(crate::core::attest::check(&attestation, &partial), Ok(()));
        let mut whole = partial.clone();
        whole.partial = None;
        assert!(crate::core::attest::check(&attestation, &whole).is_err());
    }

    #[test]
    fn only_a_reused_step_names_its_signers_each_once_by_key() {
        let receipt = Receipt::seal(identity("a", "out"), Observed::default());
        let key = crate::core::attest::public_key_of(&[7; 32]);
        let mut vouched = coherent(&receipt);
        vouched.steps[0].cached = true;
        vouched.steps[0].vouched_by = vec![key.clone()];
        vouched.spend = Spend::default();
        vouched.spend.add_reused(Observed::default().metrics);
        assert_eq!(judge(&vouched, &receipt), Vec::<String>::new());

        let mut executed = coherent(&receipt);
        executed.steps[0].vouched_by = vec![key.clone()];
        let found = judge(&executed, &receipt);
        assert!(found[0].contains("was not reused"), "{found:?}");

        let mut twice = vouched.clone();
        twice.steps[0].vouched_by = vec![key.clone(), key.clone()];
        let found = judge(&twice, &receipt);
        assert!(found[0].contains("named more than once"), "{found:?}");

        let mut shouted = vouched.clone();
        shouted.steps[0].vouched_by = vec![format!("ed25519:{}", key[8..].to_uppercase())];
        let found = judge(&shouted, &receipt);
        assert!(found[0].contains("is not a signer key"), "{found:?}");
    }

    #[test]
    fn a_manifest_must_carry_its_receipts_flow_and_limitations() {
        let mut body = identity("a", "out");
        body.limitations = vec!["inline output trimmed".into()];
        let receipt = Receipt::seal(body, Observed::default());
        let manifest = coherent(&receipt);
        assert_eq!(judge(&manifest, &receipt), Vec::<String>::new());

        let mut cleaner = manifest.clone();
        cleaner.limitations.clear();
        let found = judge(&cleaner, &receipt);
        assert!(
            found[0].contains("omits the receipt's limitation"),
            "{found:?}"
        );

        let mut renamed = manifest.clone();
        renamed.flow = "other".into();
        let found = judge(&renamed, &receipt);
        assert!(found[0].contains("belongs to flow `demo`"), "{found:?}");
    }

    #[test]
    fn a_receipt_must_be_authentic_and_point_at_this_runs_dependencies() {
        let mut body = identity("a", "out");
        body.deps = vec![DepRef {
            step_id: "elsewhere".into(),
            receipt_id: "sha256:y".into(),
        }];
        let receipt = Receipt::seal(body, Observed::default());
        let found = judge(&coherent(&receipt), &receipt);
        assert!(found[0].contains("references `elsewhere`"), "{found:?}");

        let mut edited = Receipt::seal(identity("a", "out"), Observed::default());
        let manifest = coherent(&edited);
        edited.identity.command = "echo other".into();
        let found = judge(&manifest, &edited);
        assert!(found[0].contains("does not match its id"), "{found:?}");
    }

    /// A run whose final receipt `held` came after one attempt `earlier`,
    /// with each attempt's observation and spend.
    fn retried(held: &Receipt, earlier: &Receipt) -> (RunManifest, BTreeMap<String, Observed>) {
        let mut manifest = coherent(held);
        let paid = Observed {
            metrics: Some(Metrics {
                cost_micro_usd: Some(7),
                ..Metrics::default()
            }),
            ..Observed::default()
        };
        let paid_digest = digest::bytes(b"paid");
        manifest.steps[0].attempts = vec![RunAttempt {
            receipt_id: earlier.receipt_id.clone(),
            observed_digest: paid_digest.clone(),
        }];
        manifest.spend.add_executed(paid.metrics);
        let observations = BTreeMap::from([
            (
                manifest.steps[0].observed_digest.clone(),
                Observed::default(),
            ),
            (paid_digest, paid),
        ]);
        (manifest, observations)
    }

    fn judge_all(
        manifest: &RunManifest,
        receipts: &[&Receipt],
        observations: &BTreeMap<String, Observed>,
    ) -> Vec<String> {
        let receipts = receipts
            .iter()
            .map(|receipt| (receipt.receipt_id.as_str(), *receipt))
            .collect();
        let observations = observations
            .iter()
            .map(|(digest, observed)| (digest.as_str(), observed.clone()))
            .collect();
        manifest.disagreements(&receipts, &observations)
    }

    #[test]
    fn earlier_attempts_are_judged_with_the_run() {
        let held = Receipt::seal(identity("a", "second"), Observed::default());
        let mut failed = identity("a", "first");
        failed.status = StepStatus::Failed;
        let failed = Receipt::seal(failed, Observed::default());
        let (manifest, observations) = retried(&held, &failed);
        assert_eq!(
            judge_all(&manifest, &[&held, &failed], &observations),
            Vec::<String>::new()
        );

        let found = judge_all(&manifest, &[&held], &observations);
        assert!(found[0].contains("a attempt 1: receipt"), "{found:?}");

        // What an attempt spent is part of what the run spent.
        let mut cheaper = manifest.clone();
        cheaper.spend = coherent(&held).spend;
        let found = judge_all(&cheaper, &[&held, &failed], &observations);
        assert!(found[0].contains("run spend disagrees"), "{found:?}");

        // An attempt that held would have ended the step there.
        let (manifest, observations) = retried(&held, &held);
        let found = judge_all(&manifest, &[&held], &observations);
        assert!(
            found[0].contains("it held, so the step would not have been tried again"),
            "{found:?}"
        );

        // A reused receipt was not attempted in this run at all.
        let (mut manifest, observations) = retried(&held, &failed);
        manifest.steps[0].cached = true;
        manifest.spend = Spend::default();
        manifest.spend.add_reused(Observed::default().metrics);
        manifest
            .spend
            .add_executed(observations[&digest::bytes(b"paid")].metrics);
        let found = judge_all(&manifest, &[&held, &failed], &observations);
        assert!(
            found[0].contains("reused receipt lists attempts"),
            "{found:?}"
        );

        let mut other = identity("b", "first");
        other.status = StepStatus::Failed;
        let other = Receipt::seal(other, Observed::default());
        let (manifest, observations) = retried(&held, &other);
        let found = judge_all(&manifest, &[&held, &other], &observations);
        assert!(
            found[0].contains("receipt belongs to step `b`"),
            "{found:?}"
        );
    }

    fn candidate(name: &str, stdout: &str, status: StepStatus) -> Receipt {
        let mut body = identity("a", stdout);
        body.status = status;
        body.candidate = Some(name.into());
        Receipt::seal(body, Observed::default())
    }

    /// A run that kept `kept` over `other`, each having reported a cost.
    fn contest(
        kept: &Receipt,
        kept_cost: u64,
        other: &Receipt,
        other_cost: u64,
    ) -> (RunManifest, BTreeMap<String, Observed>) {
        let mut manifest = coherent(kept);
        let costing = |cost| Observed {
            metrics: Some(Metrics {
                cost_micro_usd: Some(cost),
                ..Metrics::default()
            }),
            ..Observed::default()
        };
        let other_digest = digest::bytes(b"other");
        manifest.steps[0].candidates = vec![RunCandidate {
            name: other.identity.candidate.clone().unwrap(),
            receipt_id: other.receipt_id.clone(),
            observed_digest: other_digest.clone(),
            attempts: Vec::new(),
        }];
        manifest.spend = Spend::default();
        manifest.spend.add_executed(costing(kept_cost).metrics);
        manifest.spend.add_executed(costing(other_cost).metrics);
        let observations = BTreeMap::from([
            (
                manifest.steps[0].observed_digest.clone(),
                costing(kept_cost),
            ),
            (other_digest, costing(other_cost)),
        ]);
        (manifest, observations)
    }

    #[test]
    fn the_kept_candidate_must_follow_from_the_others() {
        let cheap = candidate("cheap", "42", StepStatus::Ok);
        let pricey = candidate("pricey", "42 too", StepStatus::Ok);
        let wrong = candidate("wrong", "41", StepStatus::Failed);

        let (manifest, observations) = contest(&cheap, 1, &pricey, 5);
        assert_eq!(
            judge_all(&manifest, &[&cheap, &pricey], &observations),
            Vec::<String>::new()
        );
        // Keeping the dearer of two that held, or one that failed over one
        // that held, is not what the rule chooses.
        let (manifest, observations) = contest(&pricey, 5, &cheap, 1);
        let found = judge_all(&manifest, &[&cheap, &pricey], &observations);
        assert!(
            found[0].contains("candidate cheap: it ranks above"),
            "{found:?}"
        );
        let (manifest, observations) = contest(&wrong, 0, &cheap, 1);
        let found = judge_all(&manifest, &[&cheap, &wrong], &observations);
        assert!(
            found[0].contains("candidate cheap: it ranks above"),
            "{found:?}"
        );

        // A loser's receipt must be that candidate's.
        let (mut manifest, observations) = contest(&cheap, 1, &pricey, 5);
        manifest.steps[0].candidates[0].name = "other".into();
        let found = judge_all(&manifest, &[&cheap, &pricey], &observations);
        assert!(
            found[0].contains("receipt is not that candidate's"),
            "{found:?}"
        );

        // A kept receipt that names no candidate cannot have rivals.
        let plain = Receipt::seal(identity("a", "plain"), Observed::default());
        let (manifest, observations) = contest(&plain, 1, &pricey, 5);
        let found = judge_all(&manifest, &[&plain, &pricey], &observations);
        assert!(found[0].contains("receipt names none"), "{found:?}");

        // Nor can two entries be the same candidate.
        let again = candidate("cheap", "42 again", StepStatus::Ok);
        let (manifest, observations) = contest(&cheap, 1, &again, 5);
        let found = judge_all(&manifest, &[&cheap, &again], &observations);
        assert!(found[0].contains("appears more than once"), "{found:?}");
    }
}
