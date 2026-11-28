//! What the commands about the workspace print: `flot init`, `flot guard`
//! and `flot doctor`.

use std::path::Path;

use crate::service::doctor::{Check, Status};
use crate::service::Guarded;

use super::super::terminal_safe;

pub(in super::super) fn render_init(path: &Path) {
    say!("wrote {}", path.display());
    say!("next: flot plan");
}

pub(in super::super) fn render_guard(
    guarded: &[Guarded],
    refused: &[Guarded],
    secrets: &[Guarded],
) {
    for found in guarded {
        say!("protected  {}: {}", found.path, found.reason);
    }
    for found in refused {
        say!("refused    {}: {}", found.path, found.reason);
    }
    for found in secrets {
        say!("secret     {}: {}", found.path, found.reason);
    }
}

pub(in super::super) fn render_doctor(checks: &[Check]) {
    for found in checks {
        let status = match found.status {
            Status::Ok => "ok",
            Status::Note => "note",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
        };
        say!("{status:<5} {:<15} {}", found.check, found.detail);
    }
}
