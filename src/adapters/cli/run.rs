//! The commands that run the flow: plan, run, and sign a run.

use std::process::ExitCode;

use anyhow::Result;

use crate::service::audit::Outcome;
use crate::service::{RunOptions, Workspace};

use super::args::{Cli, Command};
use super::render::{emit, print_json, render_plan, render_run, render_signed};
use super::{exit_for, progress};

pub(super) fn dispatch(cli: &Cli, workspace: &Workspace) -> Result<ExitCode> {
    match &cli.command {
        Command::Plan {
            only,
            force,
            proofs_from,
        } => {
            let plan = workspace.plan(&RunOptions {
                only: only.clone(),
                force: *force,
                proofs_from: proofs_from.clone(),
                ..RunOptions::default()
            })?;
            emit(cli.json, &plan, render_plan)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Run {
            only,
            force,
            jobs,
            max_cost,
            sign,
            proofs_from,
            progress: shown,
        } => {
            if *sign {
                // A missing key must stop the run before it spends anything.
                Workspace::key_show()?;
            }
            let view = progress::View::start(*shown);
            let report = workspace.run(&RunOptions {
                only: only.clone(),
                force: *force,
                jobs: jobs.map(|jobs| jobs as usize),
                max_cost_usd: *max_cost,
                proofs_from: proofs_from.clone(),
                progress: view.as_ref().map(progress::View::sink).unwrap_or_default(),
                ..RunOptions::default()
            });
            // The live view is gone before anything else is printed.
            drop(view);
            let report = report?;
            let signed = if *sign {
                Some(workspace.sign(&report.manifest.run_id)?)
            } else {
                None
            };
            if cli.json {
                let mut value = serde_json::to_value(&report)?;
                value["result"] = serde_json::to_value(Outcome::of_run(&report.manifest))?;
                if let Some(signed) = &signed {
                    value["signed"] = serde_json::to_value(signed)?;
                }
                print_json(&value)?;
            } else {
                render_run(&report);
                if let Some(signed) = &signed {
                    render_signed(signed);
                }
            }
            Ok(exit_for(Outcome::of_run(&report.manifest)))
        }
        Command::Sign { run } => {
            let signed = workspace.sign(run)?;
            emit(cli.json, &signed, render_signed)?;
            Ok(ExitCode::SUCCESS)
        }
        other => unreachable!("`dispatch` routes only plan, run and sign here, not {other:?}"),
    }
}
