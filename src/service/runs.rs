//! Planning and running the flow, and reading back what its runs recorded:
//! one run, its audit, a diff of two, a receipt, the list, a search and the
//! flaky steps.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use crate::core::attest;
use crate::core::compare::{self, RunDiff};
use crate::core::flow::Flow;
use crate::core::receipt::{ClaimCounts, ClaimRecord, Receipt, RunManifest, Verdict};
use crate::engine::baseline::Baseline;
use crate::engine::git::Git;
use crate::engine::runner::Runner;
use crate::service::audit::{self, Audit};
use crate::service::{flaky, search};
use crate::service::{PlanEntry, RunOptions, RunReport, Workspace, DEFAULT_FLOW_FILE};
use crate::store::GcReport;

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub flow: String,
    pub flow_digest: String,
    pub steps: Vec<PlanEntry>,
}

/// A recorded run with the claims each step made, read from its receipts.
#[derive(Debug, Clone, Serialize)]
pub struct RunDetail {
    #[serde(flatten)]
    pub manifest: RunManifest,
    /// The run arrived in a bundle rather than ran here.
    pub imported: bool,
    /// Claims by step id. A step whose receipt is unreadable or does not
    /// authenticate is absent here; `verify` reports why.
    pub step_claims: BTreeMap<String, Vec<ClaimRecord>>,
    /// Steps the run reused from imported runs, with who vouched for each,
    /// as [`audit::vouched`] judges them now.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub vouched: Vec<attest::VouchedStep>,
}

/// Recorded runs, newest first, and the ones that could not be read.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RunList {
    pub runs: Vec<RunSummary>,
    pub unreadable: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub verdict: Verdict,
    pub root_digest: String,
    pub claims: ClaimCounts,
    pub steps: usize,
    /// The run arrived in a bundle rather than ran here.
    pub imported: bool,
}

impl Workspace {
    pub fn plan(&self, options: &RunOptions) -> Result<Plan> {
        let flow = self.flow()?;
        let store = self.store()?;
        let baseline = self.baseline(Some(&flow), options.proofs_from.as_deref())?;
        let steps = Runner::new(&flow, &self.root, &store)
            .with_baseline(baseline.as_ref())
            .plan(options)?;
        Ok(Plan {
            flow_digest: flow.digest(),
            flow: flow.meta.name,
            steps,
        })
    }

    pub fn run(&self, options: &RunOptions) -> Result<RunReport> {
        let flow = self.flow()?;
        let store = self.store()?;
        let baseline = self.baseline(Some(&flow), options.proofs_from.as_deref())?;
        let mut report = Runner::new(&flow, &self.root, &store)
            .with_baseline(baseline.as_ref())
            .reusing_digests()
            .run(options)?;
        report.vouched = audit::vouched(&store, &report.manifest)?;
        Ok(report)
    }

    /// The revision proofs must match: `named`, else the committed flow's
    /// `proofs_from`, else the working copy's. The committed setting wins so
    /// that an edit to the working copy cannot drop it.
    pub(super) fn baseline(
        &self,
        flow: Option<&Flow>,
        named: Option<&str>,
    ) -> Result<Option<Baseline>> {
        let file = self
            .flow_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| DEFAULT_FLOW_FILE.into());
        let committed = || -> Option<String> {
            let git = Git::open(&self.root).ok()?;
            let id = git
                .run(
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("HEAD:./{file}"),
                    ],
                    None,
                )
                .ok()?;
            let text = String::from_utf8(git.blob(&id, 1 << 20).ok()?).ok()?;
            Flow::parse(&text).ok()?.meta.proofs_from
        };
        let rev = match named {
            Some(rev) => Some(rev.to_string()),
            None => committed().or_else(|| flow.and_then(|flow| flow.meta.proofs_from.clone())),
        };
        rev.map(|rev| Baseline::load(&self.root, &file, &rev))
            .transpose()
    }

    /// A recorded run: `latest`, an exact id, or an unambiguous substring.
    pub fn show(&self, run: &str) -> Result<RunDetail> {
        let store = self.store()?;
        let manifest = store.resolve_run(run)?;
        let step_claims = manifest
            .steps
            .iter()
            .filter_map(|step| {
                store
                    .load_receipt(&step.receipt_id)
                    .ok()
                    .filter(Receipt::is_authentic)
                    .map(|receipt| (step.step_id.clone(), receipt.identity.claims))
            })
            .collect();
        let vouched = audit::vouched(&store, &manifest)?;
        Ok(RunDetail {
            imported: store.is_imported(&manifest.run_id),
            manifest,
            step_claims,
            vouched,
        })
    }

    /// Re-authenticate a recorded run and, when a revision is named here or
    /// set by `proofs_from`, hold its proofs to that revision.
    pub fn verify(&self, run: &str, proofs_from: Option<&str>) -> Result<Audit> {
        let store = self.store()?;
        let manifest = store.resolve_run(run)?;
        let mut audit = audit::audit(&store, &manifest)?;
        // The flow is optional here: a run can be verified without one.
        let flow = self.flow().ok();
        if let Some(baseline) = self.baseline(flow.as_ref(), proofs_from)? {
            audit.hold_to(audit::anchor(&store, &manifest, &baseline)?);
        }
        Ok(audit)
    }

    pub fn diff(&self, left: &str, right: &str) -> Result<RunDiff> {
        let store = self.store()?;
        let (left, right) = (store.resolve_run(left)?, store.resolve_run(right)?);
        // Only a receipt whose body still hashes to its id can say why.
        let receipts = left
            .steps
            .iter()
            .chain(&right.steps)
            .filter_map(|step| store.load_receipt(&step.receipt_id).ok())
            .filter(Receipt::is_authentic)
            .map(|receipt| (receipt.receipt_id.clone(), receipt))
            .collect();
        Ok(compare::diff(&left, &right, &receipts))
    }

    /// A receipt by full id or unambiguous hex prefix. One whose body no
    /// longer hashes to its id is refused: nothing it says can be believed.
    pub fn receipt(&self, spec: &str) -> Result<Receipt> {
        let receipt = self.store()?.find_receipt(spec)?;
        if !receipt.is_authentic() {
            return Err(crate::core::fault::integrity(format!(
                "receipt {} does not match its body; it was altered after it was recorded",
                receipt.receipt_id
            )));
        }
        Ok(receipt)
    }

    /// Recorded runs, newest first.
    pub fn runs(&self, limit: usize) -> Result<RunList> {
        let store = self.store()?;
        let mut list = RunList::default();
        for run_id in store.list_runs()?.into_iter().take(limit) {
            // One unreadable run does not hide the others; it is named.
            match store.resolve_run(&run_id) {
                Ok(manifest) => list.runs.push(RunSummary {
                    imported: store.is_imported(&run_id),
                    run_id: manifest.run_id,
                    verdict: manifest.verdict,
                    root_digest: manifest.root_digest,
                    claims: manifest.claims,
                    steps: manifest.steps.len(),
                }),
                Err(error) => list.unreadable.push(format!("{run_id}: {error:#}")),
            }
        }
        Ok(list)
    }

    /// Search the newest `runs` recorded runs for `query`, literally and
    /// ignoring case; at most `limit` hits.
    pub fn search(&self, query: &str, runs: usize, limit: usize) -> Result<search::SearchReport> {
        search::search(&self.store()?, query, runs, limit)
    }

    /// Steps and proofs that gave different results on the same inputs in
    /// the newest `runs` runs executed here.
    pub fn flaky(&self, runs: usize) -> Result<flaky::FlakyReport> {
        flaky::flaky(&self.store()?, runs)
    }

    pub fn gc(&self, dry_run: bool) -> Result<GcReport> {
        self.store()?.gc(dry_run)
    }
}
