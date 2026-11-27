//! Command line surface.
//!
//! Exit codes are part of the contract, because CI reads them:
//! `0` passed, `1` failed or evidence that does not authenticate, `2`
//! completed with unverified claims or ran only some of the flow's steps,
//! `3` the request itself was invalid, `4`
//! the environment failed (IO, permissions, a busy store; retrying can help).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use crate::service::audit::Outcome;
use crate::service::bundle::ImportPolicy;
use crate::service::doctor::Status;
use crate::service::{
    fault_kind, judge, Call, FaultKind, Guarded, RunOptions, Sharing, Shell, Workspace,
};

use args::{Cli, Command, KeyAction, TrustAction};
use hook::answer_hook;
use render::{
    emit, markdown_report, needs_escape, print_json, render_audit, render_bundle, render_detail,
    render_diff, render_doctor, render_flaky, render_guard, render_import, render_init, render_key,
    render_key_rotate, render_limitations, render_plan, render_pull, render_push, render_run,
    render_runs, render_search, render_signed, render_trust_add, render_trust_list,
    render_trust_remove, render_trust_rotate,
};
use replay::replay_transcripts;

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
mod progress;
mod render;
mod replay;

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
        Command::Show { run } => {
            let detail = workspace.show(run)?;
            emit(cli.json, &detail, render_detail)?;
            Ok(exit_for(Outcome::of_run(&detail.manifest)))
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
            candidate,
            attempt,
        } => {
            use std::io::Write;
            let which = crate::service::OutputOf {
                candidate: candidate.clone(),
                attempt: attempt.map(|number| number as usize),
            };
            let stream = match (*stderr, *proof) {
                (true, _) => crate::service::Stream::Stderr,
                (_, true) => crate::service::Stream::Proof,
                _ => crate::service::Stream::Stdout,
            };
            let digest = workspace.step_output(run, step.as_deref(), &which, stream)?;
            std::io::stdout().write_all(&workspace.object(&digest)?)?;
            Ok(ExitCode::SUCCESS)
        }
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
    }
}

fn exit_for(outcome: Outcome) -> ExitCode {
    ExitCode::from(outcome.exit_code())
}

#[cfg(test)]
mod tests {
    use super::terminal_safe;

    #[test]
    fn control_sequences_and_bidi_overrides_are_made_visible() {
        assert_eq!(terminal_safe("plain\ttext\nok"), "plain\ttext\nok");
        assert_eq!(terminal_safe("\u{1b}[2Jgone"), "\\u{1b}[2Jgone");
        assert_eq!(terminal_safe("over\rwrite"), "over\\u{d}write");
        assert_eq!(terminal_safe("a\u{202E}b"), "a\\u{202e}b");
        assert_eq!(terminal_safe("한글 ✓"), "한글 ✓");
    }
}
