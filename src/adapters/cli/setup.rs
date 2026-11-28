//! The commands about the workspace rather than one run: init, doctor, the
//! guard, the MCP server and gc.

use std::process::ExitCode;

use anyhow::{Context, Result};

use crate::service::doctor::Status;
use crate::service::{judge, Call, Guarded, Shell, Workspace};

use super::args::{Cli, Command};
use super::hook::answer_hook;
use super::render::{emit, print_json, render_doctor, render_guard, render_init};
use super::replay::replay_transcripts;
use super::terminal_safe;

pub(super) fn dispatch(
    cli: &Cli,
    workspace: &Workspace,
    serve_mcp: fn(&Workspace) -> Result<()>,
) -> Result<ExitCode> {
    match &cli.command {
        Command::Init { force } => {
            let path = workspace.init(*force)?;
            render_init(&path);
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor => {
            let checks = workspace.doctor();
            emit(cli.json, &checks, |checks| render_doctor(checks))?;
            Ok(if checks.iter().any(|found| found.status == Status::Fail) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Guard {
            paths,
            command,
            powershell,
            hook,
            replay,
        } => {
            let cwd = std::env::current_dir().context("cannot read the current directory")?;
            if *replay {
                return replay_transcripts(paths, cli.json);
            }
            let call = Call {
                named: Vec::new(),
                targets: paths.iter().map(|path| cwd.join(path)).collect(),
                commands: command
                    .iter()
                    .map(|command| {
                        let language = if *powershell {
                            Shell::PowerShell
                        } else {
                            Shell::Posix
                        };
                        (cwd.clone(), command.clone(), language)
                    })
                    .collect(),
            };
            if *hook {
                return answer_hook(workspace, cli.file.is_none(), &cwd, call);
            }
            let judged = judge(workspace, cli.file.is_none(), call)?;
            let refused: Vec<Guarded> = judged
                .refused
                .into_iter()
                .map(|refused| Guarded {
                    path: refused.shape,
                    reason: refused.why,
                })
                .collect();
            if cli.json {
                print_json(&serde_json::json!({
                    "protected": judged.guarded,
                    "refused": refused,
                    "secret": judged.secrets,
                }))?;
            } else {
                render_guard(&judged.guarded, &refused, &judged.secrets);
            }
            Ok(
                if judged.guarded.is_empty() && refused.is_empty() && judged.secrets.is_empty() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(2)
                },
            )
        }
        Command::Mcp => {
            serve_mcp(workspace)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Gc { dry_run } => {
            let report = workspace.gc(*dry_run)?;
            if cli.json {
                print_json(&report)?;
            } else {
                let verb = if report.dry_run {
                    "would collect"
                } else {
                    "collected"
                };
                say!(
                    "{verb}: receipts kept {} removed {}; objects kept {} removed {}; cache entries removed {}",
                    report.kept_receipts,
                    report.removed_receipts,
                    report.kept_objects,
                    report.removed_objects,
                    report.removed_cache
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        other => unreachable!("`dispatch` routes only workspace commands here, not {other:?}"),
    }
}
