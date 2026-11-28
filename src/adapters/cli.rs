//! Command line surface.
//!
//! Exit codes are part of the contract, because CI reads them:
//! `0` passed, `1` failed or evidence that does not authenticate, `2`
//! completed with unverified claims or ran only some of the flow's steps,
//! `3` the request itself was invalid, `4`
//! the environment failed (IO, permissions, a busy store; retrying can help).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use crate::service::audit::Outcome;
use crate::service::{fault_kind, FaultKind, Workspace};

use args::{Cli, Command};
use render::needs_escape;

const INVALID_REQUEST: u8 = 3;

/// The exit code for a command that failed with `kind`.
fn exit_for_fault(kind: FaultKind) -> ExitCode {
    ExitCode::from(match kind {
        FaultKind::Integrity => 1,
        FaultKind::Invalid => INVALID_REQUEST,
        FaultKind::Environment => 4,
    })
}

/// Print a line with every terminal control sequence neutralized. Step
/// output, claim text and limitations can come from an imported bundle, and
/// an escape sequence in them must not drive the reader's terminal.
macro_rules! say {
    () => { println!() };
    ($($arg:tt)*) => { println!("{}", terminal_safe(&format!($($arg)*))) };
}

mod args;
mod hook;
mod inspect;
mod progress;
mod render;
mod replay;
mod run;
mod setup;
mod share;
#[cfg(test)]
mod tests;

/// `text` with control characters other than newline and tab, and the
/// bidirectional overrides that make text read differently than it is,
/// replaced by visible escapes.
fn terminal_safe(text: &str) -> std::borrow::Cow<'_, str> {
    let unsafe_char = needs_escape;
    if !text.chars().any(unsafe_char) {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(
        text.chars()
            .map(|c| {
                if unsafe_char(c) {
                    c.escape_unicode().to_string()
                } else {
                    c.to_string()
                }
            })
            .collect(),
    )
}

/// Parse arguments and run one command. `serve_mcp` is supplied by the binary,
/// which is the only place that knows both adapters.
pub fn main(serve_mcp: fn(&Workspace) -> Result<()>) -> ExitCode {
    // Rust ignores SIGPIPE, so `flot show | head` would panic on the closed
    // pipe. A command-line tool should just stop, like any other.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    crate::service::install_interrupt_handlers();
    // clap exits 2 on a usage error, which would read as `unverified` to CI.
    // Map it onto this CLI's own invalid-request code.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let informational = matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            );
            let _ = error.print();
            return if informational {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(INVALID_REQUEST)
            };
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let workspace = match &cli.command {
        Command::Init { .. } => Ok(Workspace::for_init(&cwd, cli.file.as_deref())),
        _ => Workspace::locate(&cwd, cli.file.as_deref()),
    };
    match workspace.and_then(|workspace| dispatch(&cli, &workspace, serve_mcp)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("flot: {}", terminal_safe(&format!("{error:#}")));
            exit_for_fault(fault_kind(&error))
        }
    }
}

fn dispatch(
    cli: &Cli,
    workspace: &Workspace,
    serve_mcp: fn(&Workspace) -> Result<()>,
) -> Result<ExitCode> {
    match &cli.command {
        Command::Init { .. }
        | Command::Doctor
        | Command::Guard { .. }
        | Command::Mcp
        | Command::Gc { .. } => setup::dispatch(cli, workspace, serve_mcp),
        Command::Plan { .. } | Command::Run { .. } | Command::Sign { .. } => {
            run::dispatch(cli, workspace)
        }
        Command::Show { .. }
        | Command::Explain { .. }
        | Command::Verify { .. }
        | Command::Diff { .. }
        | Command::Receipt { .. }
        | Command::Cat { .. }
        | Command::Log { .. }
        | Command::Flaky { .. }
        | Command::Search { .. }
        | Command::Runs { .. } => inspect::dispatch(cli, workspace),
        Command::Bundle { .. }
        | Command::Push { .. }
        | Command::Pull { .. }
        | Command::Import { .. }
        | Command::Key { .. }
        | Command::Trust { .. } => share::dispatch(cli, workspace),
    }
}

fn exit_for(outcome: Outcome) -> ExitCode {
    ExitCode::from(outcome.exit_code())
}
