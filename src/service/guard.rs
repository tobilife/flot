//! The guard: what a tool call would change or read, judged against every
//! flow it may touch: which files are a flow's evidence or decide whether it
//! is checked, which hold the signing key, which files a whole-tree git
//! command would change, and the answer the hook gives.

use serde::Serialize;

mod expand;
mod git;
mod judge;
mod paths;
mod rules;
mod verbs;

pub use expand::tilde;
pub use judge::{answer, judge, Answer, Call, Judged, Refused, Shell};

/// A file whose change would touch the flow's proofs, or a secret whose
/// reading would let an agent sign as the user, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Guarded {
    /// As the guard names it: relative to the nearest flow's directory or
    /// its repository, from `~/` in the user's home directory, else absolute.
    pub path: String,
    pub reason: String,
}
