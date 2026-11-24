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
    pub deps: Vec<DepRef>,
    pub claims: Vec<ClaimRecord>,
    /// Known gaps in this receipt's evidence, stated rather than implied.
    pub limitations: Vec<String>,
    pub tool: ToolIdentity,
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
    /// always retried. A claim with no verify command stays unverified however
    /// often it runs, so it does not block reuse.
    pub fn is_reusable(&self) -> bool {
        self.identity.status.is_ok()
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
        objects.extend(
            self.identity
                .claims
                .iter()
                .filter_map(|claim| claim.verify_output_digest.as_deref()),
        );
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunManifest {
    pub run_id: String,
    pub flow: String,
    pub flow_digest: String,
    /// Fold of the ordered step receipt ids. Deterministic flows reproduce it.
    pub root_digest: String,
    pub verdict: Verdict,
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
            let Some(receipt) = receipts.get(step.receipt_id.as_str()) else {
                found.push(format!(
                    "{}: receipt {} is not available",
                    step.step_id,
                    digest::short(&step.receipt_id)
                ));
                continue;
            };
            if receipt.identity.step_id != step.step_id {
                found.push(format!(
                    "{}: receipt belongs to step `{}`",
                    step.step_id, receipt.identity.step_id
                ));
            }
            if !receipt.is_authentic() {
                found.push(format!(
                    "{}: receipt body does not match its id {}",
                    step.step_id,
                    digest::short(&receipt.receipt_id)
                ));
            }
            for dependency in &receipt.identity.deps {
                match self.step(&dependency.step_id) {
                    Some(recorded) if recorded.receipt_id == dependency.receipt_id => {}
                    Some(_) => found.push(format!(
                        "{}: references a different receipt for `{}` than the run recorded",
                        step.step_id, dependency.step_id
                    )),
                    None => found.push(format!(
                        "{}: references `{}`, which is not in this run",
                        step.step_id, dependency.step_id
                    )),
                }
            }
            if receipt.identity.claims.iter().any(|claim| {
                claim.status == ClaimStatus::Verified && claim.verify_output_digest.is_none()
            }) {
                found.push(format!(
                    "{}: claim is marked verified with no proof object",
                    step.step_id
                ));
            }
            if receipt.identity.flow != self.flow {
                found.push(format!(
                    "{}: receipt belongs to flow `{}`",
                    step.step_id, receipt.identity.flow
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
            deps: vec![],
            claims: vec![],
            limitations: vec![],
            tool: ToolIdentity::default(),
        }
    }

    fn run_step(step: &str, status: StepStatus, claims: ClaimCounts) -> RunStep {
        RunStep {
            observed_digest: String::new(),
            step_id: step.into(),
            receipt_id: "sha256:x".into(),
            status,
            cached: false,
            claims,
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
        body.claims.push(ClaimRecord {
            verify_output_digest: Some(proof.clone()),
            ..claim(ClaimStatus::Verified, Some("true"))
        });
        let receipt = Receipt::seal(body, Observed::default());
        let objects = receipt.objects();
        assert!(objects.contains(&receipt.identity.stdout_digest.as_str()));
        assert!(objects.contains(&receipt.identity.stderr_digest.as_str()));
        assert!(objects.contains(&digest::bytes(b"out").as_str()));
        assert!(objects.contains(&proof.as_str()));
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
    }

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
        }];
        let root_digest =
            RunManifest::root_of(&[(steps[0].step_id.clone(), steps[0].receipt_id.clone())]);
        RunManifest {
            run_id: RunManifest::id_for(1, &root_digest),
            flow: receipt.identity.flow.clone(),
            flow_digest: String::new(),
            root_digest,
            verdict: verdict_for(&steps, receipt.claim_counts()),
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
}
