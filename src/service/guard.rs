//! The guard's judgment of paths: which files are a flow's evidence or
//! decide whether it is checked, which hold the signing key, and which
//! files a whole-tree git command would change.

use serde::Serialize;

mod git;
mod paths;
mod rules;

pub use git::git_would_change;
pub use rules::Guard;

/// A file whose change would touch the flow's proofs, or a secret whose
/// reading would let an agent sign as the user, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Guarded {
    /// As the guard names it: relative to the nearest flow's directory or
    /// its repository, from `~/` in the user's home directory, else absolute.
    pub path: String,
    pub reason: String,
}
