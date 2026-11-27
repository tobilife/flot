//! The command line's grammar: each command, its arguments and the help
//! text clap prints for them.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "flot",
    version,
    about = "Verifiable agent flow runtime: every step emits a receipt, every claim needs proof."
)]
pub(super) struct Cli {
    /// Flow file to use, relative to the current directory. Without it, the
    /// nearest flot.toml here or above is used. The store sits beside the flow.
    #[arg(long, short = 'f', global = true)]
    pub(super) file: Option<PathBuf>,
    /// Machine-readable output.
    #[arg(long, global = true)]
    pub(super) json: bool,
    #[command(subcommand)]
    pub(super) command: Command,
}

#[derive(Subcommand, Debug)]
pub(super) enum Command {
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
        /// Show the steps on stderr as they run: `live` redraws the running
        /// steps below their output, `plain` prints one line per event, `off`
        /// prints nothing. `auto` is live on a terminal, plain elsewhere.
        #[arg(long, value_enum, value_name = "WHEN", default_value_t = Progress::Auto)]
        progress: Progress,
    },
    /// Check what this setup relies on: the flow, proofs held to a
    /// revision, the store kept out of git, keys, trust, the sandbox, and
    /// agent settings that would switch the guard off. Exits 1 when one
    /// fails. Changes nothing.
    Doctor,
    /// Say whether changing these files would touch the flow's proofs or
    /// decide whether they are checked, or whether reading them would hand
    /// out the signing key. Exits 2 when one would. With
    /// --command, reads a shell command line (a PowerShell one with
    /// --powershell); with --hook, a Claude Code
    /// PreToolUse payload on stdin, answering with a denial on stdout and
    /// noting it in the store for the next run to record.
    Guard {
        paths: Vec<PathBuf>,
        /// Also read a shell command line for the files it writes, the
        /// scripts and interpreter code it runs, what it reads of the
        /// signing key, and downloaded code piped into an interpreter.
        #[arg(long)]
        command: Option<String>,
        /// Read --command as PowerShell, as the hook reads Claude Code's
        /// PowerShell tool.
        #[arg(long, requires = "command")]
        powershell: bool,
        #[arg(long)]
        hook: bool,
        /// Judge every edit and shell command in Claude Code transcripts
        /// (the PATHS, files or directories; by default ~/.claude/projects)
        /// as the hook would now, and list what it would refuse.
        #[arg(long, conflicts_with_all = ["command", "hook"])]
        replay: bool,
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
        /// Print a Markdown report of the run and this check, for a pull
        /// request comment or a CI job summary.
        #[arg(long)]
        markdown: bool,
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
    /// Write the complete output one step of a run printed to stdout. Without
    /// a step, name the steps of the latest run.
    Log {
        step: Option<String>,
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
        /// Share the run even if it carries credential-shaped tokens, which
        /// are otherwise refused.
        #[arg(long)]
        allow_secrets: bool,
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
        /// Share the run even if it carries credential-shaped tokens, which
        /// are otherwise refused.
        #[arg(long)]
        allow_secrets: bool,
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
    /// Name the steps and proofs that gave different results on the same
    /// inputs in runs executed here. Exits 2 when there is one.
    Flaky {
        /// How many of the newest runs to read.
        #[arg(long, default_value_t = 50)]
        runs: usize,
    },
}

/// How `flot run` shows its steps as they run.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Progress {
    Auto,
    Live,
    Plain,
    Off,
}

#[derive(Subcommand, Debug)]
pub(super) enum KeyAction {
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
pub(super) enum TrustAction {
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
