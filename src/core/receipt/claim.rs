//! What a receipt records of each claim: its verdict, and what each proof
//! the step declares showed.

use serde::{Deserialize, Serialize};

use crate::core::mutants::Survivor;

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
    /// The proof held to the files a step declares `held`, as they are at a
    /// revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<Held>,
    /// The mutants the proof was tested against, for a step that declares
    /// `mutants`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutants: Option<Mutated>,
    /// Why the proof was not run: it differed from the revision the run held
    /// proofs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub differed: Option<Differed>,
}

/// A proof that differed from the revision a run held proofs to, which the
/// run therefore did not run: the change to it is for a person to judge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Differed {
    /// The commit the revision named.
    pub commit: String,
    /// How the proof differed from it.
    pub reasons: Vec<String>,
}

/// What a mutation tester run against a step's proof found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mutated {
    /// The command that ran it.
    pub command: String,
    /// The commit it had as `$FLOT_PROOFS_FROM`, when proofs are checked
    /// against one: what it may have mutated the changes since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// How it ended; `None` when it was stopped.
    pub exit_code: Option<i32>,
    /// Digest of its combined output.
    pub output_digest: String,
    /// Digest of the report it wrote, when it wrote one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_digest: Option<String>,
    /// Mutants the proof caught.
    pub caught: u32,
    /// Mutants that could not be tested.
    pub untested: u32,
    /// Mutants the proof did not catch and no entry of the allow list allows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub survived: Vec<Survivor>,
    /// Mutants the proof did not catch that the allow list allows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<Survivor>,
}

/// What a proof showed of the files a step holds to a revision: the
/// revision's versions of them, against the work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    /// The digest of those files as the revision has them, their paths,
    /// modes and contents: what a reused receipt must still match.
    pub base: String,
    /// The files that differ here, which the proof ran with as the revision
    /// has them; empty when none did and the proof itself showed it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// How the proof ended there, when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Digest of the proof command's combined output there, when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_digest: Option<String>,
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
