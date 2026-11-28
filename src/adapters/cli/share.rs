//! The commands that share runs and manage the keys that sign them: bundle,
//! push, pull, import, key and trust.

use std::process::ExitCode;

use anyhow::Result;

use crate::service::bundle::ImportPolicy;
use crate::service::{Sharing, Workspace};

use super::args::{Cli, Command, KeyAction, TrustAction};
use super::exit_for_fault;
use super::render::{
    emit, render_bundle, render_import, render_key, render_key_rotate, render_pull, render_push,
    render_trust_add, render_trust_list, render_trust_remove, render_trust_rotate,
};

pub(super) fn dispatch(cli: &Cli, workspace: &Workspace) -> Result<ExitCode> {
    match &cli.command {
        Command::Bundle {
            run,
            out,
            sign,
            allow_secrets,
        } => {
            let share = Sharing {
                sign: *sign,
                allow_secrets: *allow_secrets,
            };
            let report = workspace.bundle(run, out.as_deref(), share)?;
            emit(cli.json, &report, render_bundle)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Push {
            run,
            remote,
            sign,
            allow_secrets,
        } => {
            let share = Sharing {
                sign: *sign,
                allow_secrets: *allow_secrets,
            };
            let report = workspace.push(run, remote, share)?;
            emit(cli.json, &report, render_push)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Pull {
            remote,
            require_trusted,
        } => {
            let report = workspace.pull(
                remote,
                ImportPolicy {
                    require_trusted: *require_trusted,
                },
            )?;
            emit(cli.json, &report, render_pull)?;
            Ok(match report.refused.first() {
                None => ExitCode::SUCCESS,
                Some(refused) => exit_for_fault(refused.kind),
            })
        }
        Command::Import {
            path,
            require_trusted,
        } => {
            let report = workspace.import(
                path,
                ImportPolicy {
                    require_trusted: *require_trusted,
                },
            )?;
            emit(cli.json, &report, render_import)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Key {
            action: KeyAction::Rotate { out },
        } => {
            let rotation = Workspace::key_rotate(out.as_deref())?;
            emit(cli.json, &rotation, render_key_rotate)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Key {
            action: action @ (KeyAction::Init | KeyAction::Show),
        } => {
            let info = match action {
                KeyAction::Init => Workspace::key_init()?,
                _ => Workspace::key_show()?,
            };
            emit(cli.json, &info, render_key)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Trust { action } => {
            match action {
                TrustAction::Add { name, key } => {
                    let entry = workspace.trust_add(name, key)?;
                    emit(cli.json, &entry, render_trust_add)?;
                }
                TrustAction::Remove { name_or_key } => {
                    let entry = workspace.trust_remove(name_or_key)?;
                    emit(cli.json, &entry, render_trust_remove)?;
                }
                TrustAction::Rotate { handover, revoke } => {
                    let rotation = workspace.trust_rotate(handover, *revoke)?;
                    emit(cli.json, &rotation, render_trust_rotate)?;
                }
                TrustAction::List => {
                    let entries = workspace.trust_list()?;
                    emit(cli.json, &entries, |entries| render_trust_list(entries))?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        other => unreachable!("`dispatch` routes only sharing commands here, not {other:?}"),
    }
}
