//! What kind of failure an error is. The CLI turns it into an exit code and
//! MCP into an error `kind`, so a person and a host learn the same fact: was
//! the request wrong, did evidence fail to authenticate, or did the machine
//! fail underneath a sound request.

use std::fmt;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind {
    /// The request cannot be served as asked: a bad flag, an unknown run, an
    /// invalid flow, a policy refusal. Asking differently can succeed.
    Invalid,
    /// Recorded or offered evidence does not authenticate: a corrupt store
    /// entry, a tampered object, a bundle that fails its checks.
    Integrity,
    /// The environment failed a sound request: IO, permissions, a busy store.
    /// Retrying can succeed.
    Environment,
}

/// An error that says which kind it is. Errors without one are classified by
/// what they wrap; see `service::fault_kind`.
#[derive(Debug)]
pub struct Fault {
    pub kind: FaultKind,
    message: String,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Fault {}

/// Evidence that does not authenticate.
pub fn integrity(message: impl fmt::Display) -> anyhow::Error {
    anyhow::Error::new(Fault {
        kind: FaultKind::Integrity,
        message: message.to_string(),
    })
}

/// A sound request the environment could not serve right now.
pub fn environment(message: impl fmt::Display) -> anyhow::Error {
    anyhow::Error::new(Fault {
        kind: FaultKind::Environment,
        message: message.to_string(),
    })
}
