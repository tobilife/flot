//! The hashed body of a receipt: what ran, over which files, after which
//! dependencies, and how it ended.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::core::flow::SCHEMA_VERSION;

use super::{ClaimRecord, FLOT_VERSION};

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
