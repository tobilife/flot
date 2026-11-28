//! Tests of claim settlement and the holds applied after a proof, with the
//! helpers they share.

use crate::core::flow::{Flow, Step};
use crate::core::receipt::{ClaimRecord, ClaimStatus};

use super::Proof;

mod before;
mod mutants;
mod settle;

fn step(extra: &str) -> Step {
    let text = format!("[flow]\nname = \"c\"\n\n[[step]]\nid = \"s\"\nrun = \"true\"\n{extra}\n");
    Flow::parse(&text).unwrap().step("s").unwrap().clone()
}

fn proof(exit_code: Option<i32>, timed_out: bool) -> Proof {
    Proof {
        exit_code,
        timed_out,
        output_digest: "sha256:proof".into(),
    }
}

fn statuses(claims: &[ClaimRecord]) -> Vec<ClaimStatus> {
    claims.iter().map(|claim| claim.status).collect()
}
