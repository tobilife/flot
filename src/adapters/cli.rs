//! Command line surface.
//!
//! Exit codes are part of the contract, because CI reads them:
//! `0` passed, `1` failed or evidence that does not authenticate, `2`
//! completed with unverified claims, `3` the request itself was invalid, `4`
//! the environment failed (IO, permissions, a busy store; retrying can help).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::core::attest::{self, Authorship, SignerCheck};
use crate::core::compare::RunDiff;
use crate::core::digest;
use crate::core::receipt::{ClaimStatus, RunManifest, StepStatus, Verdict};
use crate::service::audit::Audit;
use crate::service::bundle::ImportPolicy;
use crate::service::{
    fault_kind, FaultKind, Plan, RunDetail, RunOptions, RunReport, SignReport, Workspace,
};

const INVALID_REQUEST: u8 = 3;

/// The exit code for a command that failed with `kind`.
fn exit_for_fault(kind: FaultKind) -> ExitCode {
    ExitCode::from(match kind {
        FaultKind::Integrity => 1,
        FaultKind::Invalid => INVALID_REQUEST,
        FaultKind::Environment => 4,
    })
}

#[derive(Parser, Debug)]
#[command(
    name = "flot",
    version,
    about = "Verifiable agent flow runtime: every step emits a receipt, every claim needs proof."
)]
pub struct Cli {
    /// Flow file to use, relative to the current directory. Without it, the
    /// nearest flot.toml here or above is used. The store sits beside the flow.
    #[arg(long, short = 'f', global = true)]
    pub file: Option<PathBuf>,
    /// Machine-readable output.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Write a starter flot.toml.
    Init {
        /// Overwrite an existing flow file.
        #[arg(long)]
        force: bool,
    },
    /// Show what would run, what would be reused, and which claims lack proof.
    Plan {
        /// Limit to one step and its dependencies.
        #[arg(long)]
        only: Option<String>,
        /// Ignore cached receipts.
        #[arg(long)]
        force: bool,
        /// Check proofs against this git revision.
        #[arg(long, value_name = "REV")]
        proofs_from: Option<String>,
    },
    /// Execute the flow and record receipts.
    Run {
        #[arg(long)]
        only: Option<String>,
        #[arg(long)]
        force: bool,
        /// Run up to this many steps at once, overriding the flow's
        /// `max_parallel`. Independent steps only; dependencies still wait.
        #[arg(long, short = 'j', value_parser = clap::value_parser!(u32).range(1..))]
        jobs: Option<u32>,
        /// Stop starting new steps once executed steps have reported this many
        /// dollars through $FLOT_METRICS. Overrides the flow's `max_cost_usd`.
        #[arg(long, value_name = "USD", allow_negative_numbers = true)]
        max_cost: Option<f64>,
        /// Sign the recorded run with the host key (see `flot key init`),
        /// whatever its verdict. The key is checked before anything runs.
        #[arg(long)]
        sign: bool,
        /// Check every proof against this git revision, such as origin/main:
        /// a claim whose proof differs there stays unverified. Overrides the
        /// flow's `proofs_from`.
        #[arg(long, value_name = "REV")]
        proofs_from: Option<String>,
    },
    /// Say whether changing these files would touch the flow's proofs: the
    /// flow file, a step's verify_inputs, or the store. Exits 2 when one
    /// would. With --hook, reads a Claude Code PreToolUse payload on stdin
    /// and answers with a denial on stdout.
    Guard {
        paths: Vec<PathBuf>,
        #[arg(long)]
        hook: bool,
    },
    /// Summarize a recorded run.
    Show {
        #[arg(default_value = "latest")]
        run: String,
    },
    /// Re-authenticate a recorded run against the store.
    Verify {
        #[arg(default_value = "latest")]
        run: String,
        /// Also hold the run's proofs to this git revision, such as
        /// origin/main; exits 2 when one differs. Overrides the flow's
        /// `proofs_from`.
        #[arg(long, value_name = "REV")]
        proofs_from: Option<String>,
    },
    /// Sign a run executed here with the host key (see `flot key init`).
    Sign {
        #[arg(default_value = "latest")]
        run: String,
    },
    /// Compare two runs step by step: by default, the latest run against the
    /// one before it.
    Diff {
        #[arg(default_value = "latest~1")]
        left: String,
        #[arg(default_value = "latest")]
        right: String,
    },
    /// Print one receipt as JSON.
    Receipt { receipt_id: String },
    /// Write a stored object (complete step output) to stdout.
    Cat { object: String },
    /// Write the complete output one step of a run printed to stdout.
    Log {
        step: String,
        #[arg(default_value = "latest")]
        run: String,
        /// The step's stderr instead of its stdout.
        #[arg(long)]
        stderr: bool,
        /// Another candidate the run executed, instead of the one it kept.
        #[arg(long, value_name = "NAME")]
        candidate: Option<String>,
        /// An earlier attempt, numbered from 1, instead of the last.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
        attempt: Option<u32>,
    },
    /// Write a run's receipts and output objects to one portable file.
    Bundle {
        #[arg(default_value = "latest")]
        run: String,
        /// Destination file. Defaults to `<run-id>.flotbundle.json`.
        #[arg(long, short = 'o')]
        out: Option<PathBuf>,
        /// Sign the run with the host key (see `flot key init`).
        #[arg(long)]
        sign: bool,
    },
    /// Authenticate a bundle and absorb it into this store.
    Import {
        path: PathBuf,
        /// Refuse the bundle unless a signer on the trust list signed it.
        #[arg(long)]
        require_trusted: bool,
    },
    /// Put a run on a git remote, in the ref refs/flot/runs, beside the runs
    /// already there. Branches and the working tree are not touched.
    Push {
        #[arg(default_value = "latest")]
        run: String,
        /// The git remote: a name such as origin, or a URL.
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Sign the run with the host key first.
        #[arg(long)]
        sign: bool,
    },
    /// Import this flow's runs from a git remote's refs/flot/runs that this
    /// store does not hold yet.
    Pull {
        /// The git remote: a name such as origin, or a URL.
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Refuse every bundle a signer on the trust list did not sign.
        #[arg(long)]
        require_trusted: bool,
    },
    /// Manage the host signing key.
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
    /// Manage which signers this store believes.
    Trust {
        #[command(subcommand)]
        action: TrustAction,
    },
    /// Serve the same contract over MCP on stdio.
    Mcp,
    /// Drop receipts and objects that no recorded run references.
    Gc {
        /// Report what would be removed without deleting anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// List recorded runs, newest first.
    Runs {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Find text in what recorded runs printed, claimed and noted, newest
    /// run first. The match is literal and ignores case.
    Search {
        query: String,
        /// How many of the newest runs to search.
        #[arg(long, default_value_t = 50)]
        runs: usize,
        /// Stop after this many hits.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

#[derive(Subcommand, Debug)]
pub enum KeyAction {
    /// Create the signing key if there is none, and print its public key.
    Init,
    /// Print the public key to share with people who import your bundles.
    Show,
    /// Replace the signing key, keeping the old secret beside it, and write a
    /// handover both keys sign for everyone who trusts the old one.
    Rotate {
        /// Where to write the handover [default: flot-handover-<key>.json].
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum TrustAction {
    /// Believe a signer: `flot trust add alice ed25519:...`.
    Add { name: String, key: String },
    /// Stop believing a signer, by name or key.
    Remove { name_or_key: String },
    /// Follow a trusted signer to their new key, given the handover their
    /// `flot key rotate` wrote.
    Rotate {
        handover: PathBuf,
        /// Stop trusting the old key as well, for a key that was exposed.
        #[arg(long)]
        revoke: bool,
    },
    /// List trusted signers.
    List,
}

/// Print a line with every terminal control sequence neutralized. Step
/// output, claim text and limitations can come from an imported bundle, and
/// an escape sequence in them must not drive the reader's terminal.
macro_rules! say {
    () => { println!() };
    ($($arg:tt)*) => { println!("{}", terminal_safe(&format!($($arg)*))) };
}

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
            say!("wrote {}", path.display());
            say!("next: flot plan");
            Ok(ExitCode::SUCCESS)
        }
        Command::Guard { paths, hook } => {
            let cwd = std::env::current_dir().context("cannot read the current directory")?;
            let mut targets: Vec<PathBuf> = paths.iter().map(|path| cwd.join(path)).collect();
            if *hook {
                let mut payload = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut payload)
                    .context("cannot read the hook payload")?;
                let payload: serde_json::Value =
                    serde_json::from_str(&payload).context("the hook payload is not JSON")?;
                let base = payload["cwd"].as_str().map(PathBuf::from).unwrap_or(cwd);
                for key in ["file_path", "notebook_path"] {
                    if let Some(path) = payload["tool_input"][key].as_str() {
                        targets.push(base.join(path));
                    }
                }
            }
            // This directory's flow, and the flow nearest each file: an edit
            // in a subproject touches that subproject's proofs.
            let mut flows = vec![workspace.clone()];
            if cli.file.is_none() {
                for target in &targets {
                    let Some(dir) = target.ancestors().skip(1).find(|dir| dir.is_dir()) else {
                        continue;
                    };
                    let found = Workspace::locate(dir, None)?;
                    if !flows
                        .iter()
                        .any(|flow| flow.flow_path() == found.flow_path())
                    {
                        flows.push(found);
                    }
                }
            }
            let guarded: Vec<_> = flows.iter().flat_map(|flow| flow.guard(&targets)).collect();
            if *hook {
                // Claude Code reads a denial on stdout; the exit stays 0 so
                // the hook's `|| true` keeps a failure to run from blocking.
                if !guarded.is_empty() {
                    let reasons: Vec<String> = guarded
                        .iter()
                        .map(|found| format!("{} is protected: {}", found.path, found.reason))
                        .collect();
                    print_json(&serde_json::json!({
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "deny",
                            "permissionDecisionReason": format!(
                                "flot: {}. Changing it would void the evidence; fix the work under test instead, or ask the user to change the check.",
                                reasons.join("; ")
                            ),
                        }
                    }))?;
                }
                return Ok(ExitCode::SUCCESS);
            }
            if cli.json {
                print_json(&guarded)?;
            } else {
                for found in &guarded {
                    say!("protected  {}: {}", found.path, found.reason);
                }
            }
            Ok(if guarded.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
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
            if cli.json {
                print_json(&plan)?;
            } else {
                render_plan(&plan);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Run {
            only,
            force,
            jobs,
            max_cost,
            sign,
            proofs_from,
        } => {
            if *sign {
                // A missing key must stop the run before it spends anything.
                Workspace::key_show()?;
            }
            let report = workspace.run(&RunOptions {
                only: only.clone(),
                force: *force,
                jobs: jobs.map(|jobs| jobs as usize),
                max_cost_usd: *max_cost,
                proofs_from: proofs_from.clone(),
                ..RunOptions::default()
            })?;
            let signed = if *sign {
                Some(workspace.sign(&report.manifest.run_id)?)
            } else {
                None
            };
            if cli.json {
                let mut value = serde_json::to_value(&report)?;
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
            Ok(exit_for(report.manifest.verdict))
        }
        Command::Sign { run } => {
            let signed = workspace.sign(run)?;
            if cli.json {
                print_json(&signed)?;
            } else {
                render_signed(&signed);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Show { run } => {
            let detail = workspace.show(run)?;
            if cli.json {
                print_json(&detail)?;
            } else {
                render_detail(&detail);
            }
            Ok(exit_for(detail.manifest.verdict))
        }
        Command::Verify { run, proofs_from } => {
            let audit = workspace.verify(run, proofs_from.as_deref())?;
            if cli.json {
                print_json(&audit)?;
            } else {
                render_audit(&audit);
            }
            Ok(if !audit.intact {
                ExitCode::from(1)
            } else if audit.verdict == Verdict::Passed
                && audit.anchor.as_ref().is_some_and(|anchor| !anchor.holds())
            {
                exit_for(Verdict::Unverified)
            } else {
                exit_for(audit.verdict)
            })
        }
        Command::Diff { left, right } => {
            let diff = workspace.diff(left, right)?;
            if cli.json {
                print_json(&diff)?;
            } else {
                render_diff(&diff);
            }
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
            candidate,
            attempt,
        } => {
            use std::io::Write;
            let which = crate::service::OutputOf {
                candidate: candidate.clone(),
                attempt: attempt.map(|number| number as usize),
            };
            let digest = workspace.step_output(run, step, &which, *stderr)?;
            std::io::stdout().write_all(&workspace.object(&digest)?)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Bundle { run, out, sign } => {
            let report = workspace.bundle(run, out.as_deref(), *sign)?;
            if cli.json {
                print_json(&report)?;
            } else {
                say!(
                    "wrote {} ({} receipts, {} objects, {} bytes)  root {}",
                    report.path.display(),
                    report.receipts,
                    report.objects,
                    report.bytes,
                    digest::short(&report.root_digest)
                );
                if report.signed_by.is_empty() {
                    say!("unsigned: importers can check integrity but not who produced it");
                }
                for key in &report.signed_by {
                    say!("signed by {key}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Push { run, remote, sign } => {
            let report = workspace.push(run, remote, *sign)?;
            if cli.json {
                print_json(&report)?;
            } else if report.unchanged {
                say!(
                    "run {} is already on {} as {}",
                    report.run_id,
                    report.remote,
                    report.path
                );
            } else {
                say!(
                    "pushed run {} to {} {}  {}  commit {}",
                    report.run_id,
                    report.remote,
                    crate::service::share::RUNS_REF,
                    report.path,
                    &report.commit[..report.commit.len().min(12)]
                );
                if report.signed_by.is_empty() {
                    say!("unsigned: others can inspect it, but only a signed run can stand in for their work");
                }
            }
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
            if cli.json {
                print_json(&report)?;
            } else if report.empty {
                say!(
                    "{} has no shared runs ({})",
                    report.remote,
                    crate::service::share::RUNS_REF
                );
            } else {
                for imported in &report.imported {
                    say!(
                        "imported run {}  root {}  {}{}",
                        imported.run_id,
                        digest::short(&imported.root_digest),
                        imported.authorship.label(),
                        if imported.reusable { "  reusable" } else { "" }
                    );
                }
                for refused in &report.refused {
                    say!("refused  {}: {}", refused.path, refused.reason);
                }
                say!(
                    "{} of flow {}: {} imported, {} already here, {} refused",
                    report.remote,
                    report.flow,
                    report.imported.len(),
                    report.present.len(),
                    report.refused.len()
                );
            }
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
            if cli.json {
                print_json(&report)?;
            } else {
                say!(
                    "imported run {}  root {}  {} receipts, {} objects; {} of them already present",
                    report.run_id,
                    digest::short(&report.root_digest),
                    report.receipts,
                    report.objects,
                    report.already_present
                );
                render_authorship(report.authorship, &report.signers);
                if report.reusable {
                    say!("reusable: local runs may reuse these receipts and their outputs");
                } else {
                    say!(
                        "not reusable: only a trusted signer's receipts can stand in for local work"
                    );
                }
                say!("next: flot verify {}", report.run_id);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Key {
            action: KeyAction::Rotate { out },
        } => {
            let rotation = Workspace::key_rotate(out.as_deref())?;
            if cli.json {
                print_json(&rotation)?;
            } else {
                say!("rotated signing key {}", rotation.path.display());
                say!("new  {}", rotation.new_key);
                say!("old  {}", rotation.old_key);
                say!("     kept as {}", rotation.retired.display());
                say!("handover {}", rotation.handover.display());
                say!("next: send it to everyone who trusts the old key; they run `flot trust rotate <file>`");
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Key {
            action: action @ (KeyAction::Init | KeyAction::Show),
        } => {
            let info = match action {
                KeyAction::Init => Workspace::key_init()?,
                _ => Workspace::key_show()?,
            };
            if cli.json {
                print_json(&info)?;
            } else {
                let state = if info.created { "created" } else { "using" };
                say!("{state} signing key {}", info.path.display());
                say!("{}", info.public_key);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Trust { action } => {
            match action {
                TrustAction::Add { name, key } => {
                    let entry = workspace.trust_add(name, key)?;
                    if cli.json {
                        print_json(&entry)?;
                    } else {
                        say!("trusting {} as {}", entry.key, entry.name);
                    }
                }
                TrustAction::Remove { name_or_key } => {
                    let entry = workspace.trust_remove(name_or_key)?;
                    if cli.json {
                        print_json(&entry)?;
                    } else {
                        say!("no longer trusting {} ({})", entry.name, entry.key);
                    }
                }
                TrustAction::Rotate { handover, revoke } => {
                    let rotation = workspace.trust_rotate(handover, *revoke)?;
                    if cli.json {
                        print_json(&rotation)?;
                    } else {
                        let state = if rotation.added {
                            "trusting"
                        } else {
                            "already trusting"
                        };
                        say!("{state} {} as {}", rotation.to, rotation.name);
                        if rotation.revoked {
                            say!("no longer trusting {}", rotation.from);
                        } else {
                            say!(
                                "still trusting {} as {}; `flot trust remove {}` once its runs no longer matter",
                                rotation.from,
                                rotation.name,
                                rotation.from
                            );
                        }
                    }
                }
                TrustAction::List => {
                    let entries = workspace.trust_list()?;
                    if cli.json {
                        print_json(&entries)?;
                    } else if entries.is_empty() {
                        say!("no trusted signers");
                    } else {
                        for entry in &entries {
                            say!("{:<16} {}", entry.name, entry.key);
                        }
                    }
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
        Command::Search { query, runs, limit } => {
            let report = workspace.search(query, *runs, *limit)?;
            if cli.json {
                print_json(&report)?;
            } else {
                for hit in &report.hits {
                    let mut place = hit.step_id.clone().unwrap_or_else(|| "(run)".into());
                    if let Some(candidate) = &hit.candidate {
                        place.push_str(&format!(" candidate {candidate}"));
                    }
                    if let Some(attempt) = hit.attempt {
                        place.push_str(&format!(" attempt {attempt}"));
                    }
                    let source = match hit.line {
                        Some(line) => format!("{}:{line}", hit.source),
                        None => hit.source.to_string(),
                    };
                    say!("{}  {place}  {source}  {}", hit.run_id, hit.text);
                }
                say!(
                    "{} hit(s) in {} run(s) searched{}",
                    report.hits.len(),
                    report.runs_searched,
                    if report.truncated {
                        "; more hits exist, raise --limit"
                    } else {
                        ""
                    }
                );
                for unreadable in &report.unreadable {
                    say!("unreadable  {unreadable}");
                }
            }
            Ok(if report.unreadable.is_empty() {
                ExitCode::SUCCESS
            } else {
                exit_for_fault(FaultKind::Integrity)
            })
        }
        Command::Runs { limit } => {
            let list = workspace.runs(*limit)?;
            if cli.json {
                print_json(&list)?;
            } else if list.runs.is_empty() && list.unreadable.is_empty() {
                say!("no runs recorded");
            } else {
                for run in &list.runs {
                    say!(
                        "{}  {:<10} {} steps  claims {}v/{}u/{}r{}",
                        run.run_id,
                        run.verdict.label(),
                        run.steps,
                        run.claims.verified,
                        run.claims.unverified,
                        run.claims.refuted,
                        if run.imported { "  (imported)" } else { "" }
                    );
                }
                for unreadable in &list.unreadable {
                    say!("unreadable  {unreadable}");
                }
            }
            // A run that cannot be read is a store that no longer holds up.
            Ok(if list.unreadable.is_empty() {
                ExitCode::SUCCESS
            } else {
                exit_for_fault(FaultKind::Integrity)
            })
        }
    }
}

fn exit_for(verdict: Verdict) -> ExitCode {
    ExitCode::from(verdict.exit_code() as u8)
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", json_text(value)?);
    Ok(())
}

/// Pretty JSON that is also safe on a terminal. serde_json escapes only C0
/// controls, so DEL, C1 controls and bidi overrides would reach the terminal
/// raw. Such characters can only occur inside JSON strings, where a `\uXXXX`
/// escape means the same character, so escaping them keeps the document
/// byte-for-byte equivalent to a JSON reader.
fn json_text<T: Serialize>(value: &T) -> Result<String> {
    let text = serde_json::to_string_pretty(value)?;
    if !text.chars().any(needs_escape) {
        return Ok(text);
    }
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if needs_escape(c) {
            escaped.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            escaped.push(c);
        }
    }
    Ok(escaped)
}

/// Characters a terminal could act on, or that make text read differently
/// than it is. Newline and tab are layout, not control.
fn needs_escape(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn render_plan(plan: &Plan) {
    say!("flow {} ({})", plan.flow, digest::short(&plan.flow_digest));
    for entry in &plan.steps {
        let reuse = if entry.cached { "reuse" } else { "run" };
        let proof = match &entry.verified_by {
            Some(command) => format!("verify `{command}`"),
            None if entry.claims > 0 => "NO PROOF".to_string(),
            None => "no claims".to_string(),
        };
        say!(
            "  {reuse:<5} {:<20} inputs {:<3} claims {:<3} {proof}",
            entry.step_id,
            entry.inputs,
            entry.claims
        );
        if !entry.outputs.is_empty() {
            say!("        > produces {}", entry.outputs.join(", "));
        }
        for limitation in &entry.limitations {
            say!("        ! {limitation}");
        }
    }
}

fn render_run(report: &RunReport) {
    for step in &report.steps {
        let mode = if step.cached {
            "cached"
        } else if step.status == StepStatus::Cancelled {
            "skipped"
        } else {
            "ran"
        };
        say!(
            "{:<8} {:<20} {:<9} {}ms  claims {}v/{}u/{}r  receipt {}{}{}",
            mode,
            step.step_id,
            step.status.label(),
            step.duration_ms,
            step.claims.verified,
            step.claims.unverified,
            step.claims.refuted,
            digest::short(&step.receipt_id),
            step.candidate
                .as_ref()
                .map(|name| format!("  candidate {name}"))
                .unwrap_or_default(),
            if step.attempts > 1 {
                format!("  attempt {}", step.attempts)
            } else {
                String::new()
            }
        );
        for output in &step.outputs {
            say!("    > {}  {}", output.path, digest::short(&output.digest));
        }
        // A reused step's output was shown when it ran; `flot show` and
        // `flot cat` still have it.
        if step.cached {
            continue;
        }
        if !step.stdout_inline.trim().is_empty() {
            for line in step.stdout_inline.lines() {
                say!("    | {line}");
            }
        }
        if !step.stderr_inline.trim().is_empty() {
            for line in step.stderr_inline.lines() {
                say!("    ! {line}");
            }
        }
        if step.stdout_truncated || step.stderr_truncated {
            say!(
                "    complete output: flot log {}{} {}",
                step.step_id,
                if step.stdout_truncated {
                    ""
                } else {
                    " --stderr"
                },
                report.manifest.run_id
            );
        }
    }
    let manifest = &report.manifest;
    say!();
    say!(
        "{} {}  root {}  {}ms",
        manifest.verdict.label().to_uppercase(),
        manifest.run_id,
        digest::short(&manifest.root_digest),
        manifest.duration_ms
    );
    render_spend(manifest);
    render_limitations(manifest);
}

fn render_spend(manifest: &RunManifest) {
    let spend = &manifest.spend;
    if spend.reporting_steps == 0
        && spend.reused_cost_micro_usd == 0
        && manifest.max_cost_micro_usd.is_none()
    {
        return;
    }
    let mut line = format!(
        "spent {} (in {} / out {} tokens, {} reporting step(s)",
        crate::core::metrics::format_usd(spend.cost_micro_usd),
        spend.input_tokens,
        spend.output_tokens,
        spend.reporting_steps
    );
    if spend.silent_steps > 0 {
        line.push_str(&format!(", {} silent", spend.silent_steps));
    }
    if spend.unpriced_steps > 0 {
        line.push_str(&format!(", {} without cost", spend.unpriced_steps));
    }
    line.push(')');
    if spend.reused_cost_micro_usd > 0 {
        line.push_str(&format!(
            "  reused {} of earlier spend",
            crate::core::metrics::format_usd(spend.reused_cost_micro_usd)
        ));
    }
    if let Some(ceiling) = manifest.max_cost_micro_usd {
        line.push_str(&format!(
            "  ceiling {}",
            crate::core::metrics::format_usd(ceiling)
        ));
    }
    say!("{line}");
}

/// The run's limitations, with one line for a message several steps share
/// ("a, b: step declares no inputs; …") rather than one line per step.
fn render_limitations(manifest: &RunManifest) {
    if manifest.limitations.is_empty() {
        return;
    }
    let mut lines: Vec<(Vec<&str>, &str)> = Vec::new();
    for limitation in &manifest.limitations {
        let by_step = limitation
            .split_once(": ")
            .filter(|(step, _)| manifest.steps.iter().any(|entry| entry.step_id == *step));
        match by_step {
            Some((step, message)) => {
                match lines
                    .iter_mut()
                    .find(|(steps, seen)| !steps.is_empty() && *seen == message)
                {
                    Some((steps, _)) => steps.push(step),
                    None => lines.push((vec![step], message)),
                }
            }
            None => lines.push((Vec::new(), limitation)),
        }
    }
    say!("limitations:");
    for (steps, message) in lines {
        if steps.is_empty() {
            say!("  - {message}");
        } else {
            say!("  - {}: {message}", steps.join(", "));
        }
    }
}

fn render_detail(detail: &RunDetail) {
    let manifest = &detail.manifest;
    say!(
        "run {}  flow {}  root {}{}",
        manifest.run_id,
        manifest.flow,
        digest::short(&manifest.root_digest),
        if detail.imported { "  (imported)" } else { "" }
    );
    for step in &manifest.steps {
        say!(
            "  {:<20} {:<9} {}  receipt {}",
            step.step_id,
            step.status.label(),
            if step.cached { "cached" } else { "ran   " },
            digest::short(&step.receipt_id)
        );
        for (number, attempt) in step.attempts.iter().enumerate() {
            say!(
                "      attempt {}  receipt {}  did not hold",
                number + 1,
                digest::short(&attempt.receipt_id)
            );
        }
        for candidate in &step.candidates {
            say!(
                "      candidate {}  receipt {}  not kept",
                candidate.name,
                digest::short(&candidate.receipt_id)
            );
        }
        if let Some(claims) = detail.step_claims.get(&step.step_id) {
            for claim in claims {
                let mark = match claim.status {
                    ClaimStatus::Verified => "+",
                    ClaimStatus::Unverified => "?",
                    ClaimStatus::Refuted => "x",
                };
                say!("      {mark} [{}] {}", claim.status.label(), claim.text);
                if let Some(reason) = &claim.reason {
                    say!("        reason: {reason}");
                }
            }
        }
        if matches!(step.status, StepStatus::Failed | StepStatus::Timeout) {
            say!(
                "      output: flot log {} {}",
                step.step_id,
                manifest.run_id
            );
        }
    }
    say!(
        "verdict {}  claims {}v/{}u/{}r  concurrency {}",
        manifest.verdict.label(),
        manifest.claims.verified,
        manifest.claims.unverified,
        manifest.claims.refuted,
        manifest.max_parallel
    );
    render_spend(manifest);
    render_limitations(manifest);
}

fn render_audit(audit: &Audit) {
    say!(
        "run {}  receipts checked {}  root {}",
        audit.run_id,
        audit.checked_receipts,
        digest::short(&audit.root_digest)
    );
    if audit.intact {
        say!("integrity: intact");
    } else {
        say!("integrity: BROKEN");
        for finding in &audit.findings {
            say!("  - {finding}");
        }
    }
    say!("verdict {}", audit.verdict.label());
    if let Some(anchor) = &audit.anchor {
        if anchor.holds() {
            say!("proofs: as at {}", anchor.baseline);
        } else {
            say!("proofs: NOT as at {}", anchor.baseline);
            for drift in &anchor.drifted {
                say!("  - {}: {}", drift.step_id, drift.reasons.join(", "));
            }
            if !anchor.missing.is_empty() {
                say!(
                    "  - claims there that this run does not make: {}",
                    anchor.missing.join(", ")
                );
            }
        }
    }
    render_authorship(audit.authorship, &audit.signers);
}

fn render_signed(signed: &SignReport) {
    say!(
        "{} run {}  root {}  by {}",
        if signed.added {
            "signed"
        } else {
            "already signed"
        },
        signed.run_id,
        digest::short(&signed.root_digest),
        signed.signer
    );
}

fn render_authorship(authorship: Authorship, signers: &[SignerCheck]) {
    say!("authorship: {}", authorship.label());
    for signer in signers {
        match (&signer.problem, &signer.trusted_as) {
            (Some(problem), _) => say!("  x {}  {problem}", attest::fingerprint(&signer.key)),
            (None, Some(name)) => say!(
                "  + {}  trusted as {name}",
                attest::fingerprint(&signer.key)
            ),
            (None, None) => say!(
                "  ? {}  not on the trust list",
                attest::fingerprint(&signer.key)
            ),
        }
    }
}

fn render_diff(diff: &RunDiff) {
    say!("left  {}  {}", diff.left, diff.left_verdict.label());
    say!("right {}  {}", diff.right, diff.right_verdict.label());
    if !diff.same_flow {
        say!("flow definition differs between the two runs");
    }
    if diff.identical {
        say!("identical: same root digest, step for step");
        return;
    }
    for step in &diff.steps {
        say!(
            "  {:<20} {:<24} {} -> {}",
            step.step_id,
            step.change,
            step.left.clone().unwrap_or_else(|| "-".into()),
            step.right.clone().unwrap_or_else(|| "-".into())
        );
    }
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
