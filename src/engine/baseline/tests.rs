//! How a proof compares with a revision that lacks it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use super::Baseline;
use crate::core::flow::Flow;

const STEP: &str = "[flow]\nname = \"f\"\n\n[[step]]\nid = \"ID\"\nrun = \"true\"\nclaims = [\"it ran\"]\nverify = \"true\"\n";

fn baseline(flow: Option<Flow>) -> Baseline {
    Baseline {
        rev: "main".into(),
        commit: "c".repeat(40),
        root: PathBuf::from("."),
        flow_file: "flot.toml".into(),
        flow,
        tree: BTreeMap::new(),
        digests: Mutex::new(BTreeMap::new()),
    }
}

#[test]
fn a_proof_names_what_the_revision_lacks() {
    let flow = Flow::parse(&STEP.replace("ID", "new")).unwrap();
    let step = flow.step("new").unwrap();
    let reasons = |baseline: &Baseline| baseline.drift(step, &[]).unwrap().unwrap().reasons;
    let other = Flow::parse(&STEP.replace("ID", "old")).unwrap();
    assert_eq!(reasons(&baseline(Some(other))), ["step `new` is not there"]);
    assert_eq!(reasons(&baseline(None)), ["the flow file is not there"]);
}
