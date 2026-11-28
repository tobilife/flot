//! The commands that read recorded runs: show, explain, verify, diff,
//! receipt, cat, log, flaky, search and runs.

use std::process::ExitCode;

use anyhow::Result;

use crate::service::audit::Outcome;
use crate::service::{FaultKind, Workspace};

use super::args::{Cli, Command};
use super::render::{
    emit, markdown_report, print_json, render_audit, render_detail, render_diff,
    render_explanation, render_flaky, render_limitations, render_runs, render_search,
};
use super::{exit_for, exit_for_fault};

pub(super) fn dispatch(cli: &Cli, workspace: &Workspace) -> Result<ExitCode> {
    match &cli.command {
        Command::Show { run } => {
            let detail = workspace.show(run)?;
            emit(cli.json, &detail, render_detail)?;
            Ok(exit_for(Outcome::of_run(&detail.manifest)))
        }
        Command::Explain { run, step } => {
            let explanation = workspace.explain(run, step.as_deref())?;
            emit(cli.json, &explanation, render_explanation)?;
            Ok(exit_for(explanation.result))
        }
        Command::Verify {
            run,
            proofs_from,
            markdown,
        } => {
            if *markdown && cli.json {
                anyhow::bail!("--markdown and --json each choose the output; name one");
            }
            let audit = workspace.verify(run, proofs_from.as_deref())?;
            if *markdown {
                print!(
                    "{}",
                    markdown_report(&audit, &workspace.show(&audit.run_id)?)
                );
            } else if cli.json {
                print_json(&audit)?;
            } else {
                render_audit(&audit);
                // What the run itself says it could not prove; a store too
                // broken to read it has said so above.
                if let Ok(detail) = workspace.show(&audit.run_id) {
                    render_limitations(&detail.manifest);
                }
            }
            Ok(exit_for(audit.result()))
        }
        Command::Diff { left, right } => {
            let diff = workspace.diff(left, right)?;
            emit(cli.json, &diff, render_diff)?;
            Ok(if diff.identical {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
        }
        Command::Receipt { receipt_id } => {
            print_json(&workspace.receipt(receipt_id)?)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Cat { object } => {
            use std::io::Write;
            std::io::stdout().write_all(&workspace.object(object)?)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Log {
            step,
            run,
            stderr,
            proof,
            held,
            mutants,
            candidate,
            attempt,
        } => {
            use std::io::Write;
            let which = crate::service::OutputOf {
                candidate: candidate.clone(),
                attempt: attempt.map(|number| number as usize),
            };
            let stream = match (*stderr, *proof, *held, *mutants) {
                (true, _, _, _) => crate::service::Stream::Stderr,
                (_, true, _, _) => crate::service::Stream::Proof,
                (_, _, true, _) => crate::service::Stream::Held,
                (_, _, _, true) => crate::service::Stream::Mutants,
                _ => crate::service::Stream::Stdout,
            };
            let digest = workspace.step_output(run, step.as_deref(), &which, stream)?;
            std::io::stdout().write_all(&workspace.object(&digest)?)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Flaky { runs } => {
            let report = workspace.flaky(*runs)?;
            emit(cli.json, &report, render_flaky)?;
            Ok(if report.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
        }
        Command::Search { query, runs, limit } => {
            let report = workspace.search(query, *runs, *limit)?;
            emit(cli.json, &report, render_search)?;
            Ok(if report.unreadable.is_empty() {
                ExitCode::SUCCESS
            } else {
                exit_for_fault(FaultKind::Integrity)
            })
        }
        Command::Runs { limit } => {
            let list = workspace.runs(*limit)?;
            emit(cli.json, &list, render_runs)?;
            // A run that cannot be read is a store that no longer holds up.
            Ok(if list.unreadable.is_empty() {
                ExitCode::SUCCESS
            } else {
                exit_for_fault(FaultKind::Integrity)
            })
        }
        other => unreachable!("`dispatch` routes only reading commands here, not {other:?}"),
    }
}
