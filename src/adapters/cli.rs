//! Command line surface.
//!
//! Exit codes are part of the contract, because CI reads them:
//! `0` passed, `1` failed or evidence that does not authenticate, `2`
//! completed with unverified claims or ran only some of the flow's steps,
//! `3` the request itself was invalid, `4`
//! the environment failed (IO, permissions, a busy store; retrying can help).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::core::attest::{self, Authorship, SignerCheck, VouchedStep, Voucher};
use crate::core::compare::RunDiff;
use crate::core::digest;
use crate::core::receipt::{ClaimStatus, Partial, RunManifest, StepStatus, Verdict};
use crate::core::{powershell, shell};
use crate::service::audit::{Audit, Outcome};
use crate::service::bundle::ImportPolicy;
use crate::service::doctor::Status;
use crate::service::{
    fault_kind, FaultKind, Guarded, Plan, RunDetail, RunOptions, RunReport, Sharing, SignReport,
    Workspace,
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
    /// Check what this setup relies on: the flow, proofs held to a
    /// revision, the store kept out of git, keys, trust, the sandbox, and
    /// agent settings that would switch the guard off. Exits 1 when one
    /// fails. Changes nothing.
    Doctor,
    /// Say whether changing these files would touch the flow's proofs or
    /// decide whether they are checked. Exits 2 when one would. With
    /// --command, reads a shell command line (a PowerShell one with
    /// --powershell); with --hook, a Claude Code
    /// PreToolUse payload on stdin, answering with a denial on stdout and
    /// noting it in the store for the next run to record.
    Guard {
        paths: Vec<PathBuf>,
        /// Also read a shell command line for the files it writes and for
        /// downloaded code piped into an interpreter.
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

/// The user's home directory, which `~` and `$HOME` name.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// `path` with a leading `~` as the home directory, as a shell expands it.
fn tilde(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), home_dir()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(path),
    }
}

/// Why downloaded code piped into an interpreter is refused.
const REMOTE: &str = "it runs code fetched from the network, which no step or receipt records";

/// What a `flot` command the guard leaves to the user does.
const VERB: &str = "changes whose evidence counts";

/// The tools the Claude Code hook reads by their kind: an edit names its
/// file, a shell its command line. Any other tool is read for the paths its
/// input names (see [`PATH_KEYS`]), unless it is named like a read.
const GUARDED_TOOLS: [&str; 6] = [
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "Bash",
    "PowerShell",
];

/// The keys whose string values, at any depth of another tool's input, are
/// paths it may change. A `source` is read by a copy but removed by a move.
const PATH_KEYS: [&str; 9] = [
    "path",
    "file_path",
    "file",
    "filename",
    "destination",
    "target",
    "notebook_path",
    "paths",
    "source",
];

/// How many paths one written word may expand to.
const EXPANSION_LIMIT: usize = 1000;

/// Whether `tool` is named like one that only reads, such as `Read`, `LS`,
/// `WebFetch` or `mcp__filesystem__list_directory`: a word of its name
/// starts with read, get, list, search, view, find, grep, glob or fetch, or
/// is ls, tree, stat or info, and none names a change, as in
/// `search_replace` or `read_and_write`.
fn reads_only(tool: &str) -> bool {
    const READS: [&str; 9] = [
        "read", "get", "list", "search", "view", "find", "grep", "glob", "fetch",
    ];
    const CHANGES: [&str; 33] = [
        "write", "edit", "create", "delete", "remove", "move", "rename", "replace", "update",
        "set", "put", "patch", "apply", "append", "insert", "save", "upload", "download", "copy",
        "mkdir", "rm", "mv", "cp", "touch", "exec", "run", "modify", "change", "add", "restore",
        "reset", "link", "unlink",
    ];
    let words = words_of(tool);
    words.iter().any(|word| {
        matches!(word.as_str(), "ls" | "tree" | "stat" | "info")
            || READS.iter().any(|read| word.starts_with(read))
    }) && !words
        .iter()
        .any(|word| CHANGES.iter().any(|change| word.starts_with(change)))
}

/// `name` in lowercase words, split at what is not a letter or digit and
/// where camelCase turns upper.
fn words_of(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut after_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            after_lower = false;
            continue;
        }
        if c.is_uppercase() && after_lower {
            words.push(std::mem::take(&mut word));
        }
        after_lower = c.is_lowercase() || c.is_ascii_digit();
        word.extend(c.to_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Every string under a key in [`PATH_KEYS`] in a tool's `input`, at any
/// depth.
fn named_paths(value: &serde_json::Value, depth: u8, found: &mut dyn FnMut(&str)) {
    fn strings(value: &serde_json::Value, found: &mut dyn FnMut(&str)) {
        match value {
            serde_json::Value::String(text) if !text.is_empty() => found(text),
            serde_json::Value::Array(items) => {
                for item in items {
                    strings(item, found);
                }
            }
            _ => {}
        }
    }
    if depth > 32 {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if PATH_KEYS.contains(&key.as_str()) {
                    strings(value, found);
                }
                named_paths(value, depth + 1, found);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                named_paths(item, depth + 1, found);
            }
        }
        _ => {}
    }
}

/// The language a command line is read in.
#[derive(Clone, Copy)]
enum Shell {
    Posix,
    PowerShell,
}

/// What one tool call would change: files it names, and command lines with
/// the directory they run in and their shell.
#[derive(Default)]
struct Call {
    targets: Vec<PathBuf>,
    commands: Vec<(PathBuf, String, Shell)>,
}

impl Call {
    /// A `tool`'s input as Claude Code gives it, run from `base`. An edit or
    /// a shell call without the file or command line its tool always names
    /// cannot be judged, which is an error.
    fn of(base: &Path, tool: &str, input: &serde_json::Value) -> Result<Call> {
        let mut call = Call::default();
        let named = |key: &str| {
            input[key]
                .as_str()
                .with_context(|| format!("the {tool} call names no {key}"))
        };
        match tool {
            "Edit" | "Write" | "MultiEdit" => {
                call.targets.push(base.join(tilde(named("file_path")?)))
            }
            "NotebookEdit" => call.targets.push(base.join(tilde(named("notebook_path")?))),
            "Bash" | "PowerShell" => {
                let shell = if tool == "PowerShell" {
                    Shell::PowerShell
                } else {
                    Shell::Posix
                };
                call.commands
                    .push((base.to_path_buf(), named("command")?.to_string(), shell));
            }
            _ if reads_only(tool) => {}
            _ => named_paths(input, 0, &mut |path| {
                call.targets.push(base.join(tilde(path)));
            }),
        }
        Ok(call)
    }

    fn is_empty(&self) -> bool {
        self.targets.is_empty() && self.commands.is_empty()
    }
}

/// A command the guard refuses whatever files it names.
struct Refused {
    /// The command as spelled, such as `curl | sh` or `flot gc`.
    shape: String,
    /// Why, said of the command.
    why: String,
    /// The sentence it is noted and denied with.
    said: String,
    /// What to do instead, which follows `said` in a denial.
    instead: &'static str,
}

impl Refused {
    fn remote(shape: String) -> Self {
        Refused {
            why: REMOTE.to_string(),
            said: format!("`{shape}` is refused: {REMOTE}"),
            instead: ". Download the file, read it, and run it as its own command, or ask the user",
            shape,
        }
    }

    fn verb(shape: String) -> Self {
        Refused {
            why: format!("it {VERB}; ask the user to run it"),
            said: format!("`{shape}` {VERB}"),
            instead: "; ask the user to run it",
            shape,
        }
    }

    /// A whole-tree git command git cannot say the files of.
    fn unknowable(shape: String, error: &anyhow::Error) -> Self {
        let why = format!(
            "git cannot say which files it would change ({error:#}), so it may change a protected one"
        );
        Refused {
            said: format!("`{shape}` is refused: {why}"),
            why,
            instead: "; ask the user to run it",
            shape,
        }
    }
}

/// What the guard refuses of one call, and the flows it consulted.
struct Judged {
    guarded: Vec<Guarded>,
    /// Downloaded code piped into an interpreter, `flot` commands left to
    /// the user, git commands whose files git cannot name.
    refused: Vec<Refused>,
    /// The flows found, each with its flow file.
    flows: Vec<Workspace>,
}

impl Judged {
    /// Each refusal in a sentence: the protected files first, then the
    /// refused commands.
    fn reasons(&self) -> Vec<String> {
        self.guarded
            .iter()
            .map(|found| format!("{} is protected: {}", found.path, found.reason))
            .chain(self.refused.iter().map(|refused| refused.said.clone()))
            .collect()
    }

    /// The denial Claude Code shows the agent.
    fn denial(&self) -> String {
        let reasons = self.reasons();
        let mut said: Vec<String> = Vec::new();
        if !self.guarded.is_empty() {
            said.push(format!(
                "{}. Changing it would void the evidence; fix the work under test instead, or ask the user to change the check",
                reasons[..self.guarded.len()].join("; ")
            ));
        }
        for refused in &self.refused {
            said.push(format!("{}{}", refused.said, refused.instead));
        }
        format!("flot: {}.", said.join(". "))
    }
}

/// One simple command of a command line: its words, quotes removed, and
/// where it is in the line.
struct Simple {
    words: Vec<String>,
    span: std::ops::Range<usize>,
}

/// The simple commands of `command`, split on unquoted `;`, `&`, `|`,
/// newlines, parentheses, braces and, in a POSIX shell, backquotes. Close
/// enough to find a program and what follows it, and no more: what is
/// found here is only ever a reason to refuse.
fn simple_commands(command: &str, language: Shell) -> Vec<Simple> {
    let escape = match language {
        Shell::Posix => '\\',
        Shell::PowerShell => '`',
    };
    let mut simples = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut start = 0;
    let mut chars = command.char_indices();
    let end_word = |word: &mut String, quoted: &mut bool, words: &mut Vec<String>| {
        if !word.is_empty() || *quoted {
            words.push(std::mem::take(word));
        }
        *quoted = false;
    };
    while let Some((at, c)) = chars.next() {
        let separates = matches!(c, ';' | '&' | '|' | '\n' | '(' | ')' | '{' | '}')
            || (c == '`' && matches!(language, Shell::Posix));
        if c == '\'' {
            quoted = true;
            for (_, c) in chars.by_ref() {
                if c == '\'' {
                    break;
                }
                word.push(c);
            }
        } else if c == '"' {
            quoted = true;
            while let Some((_, c)) = chars.next() {
                if c == '"' {
                    break;
                }
                if c == escape {
                    word.extend(chars.next().map(|(_, next)| next));
                } else {
                    word.push(c);
                }
            }
        } else if c == escape {
            word.extend(
                chars
                    .next()
                    .map(|(_, next)| next)
                    .filter(|next| *next != '\n'),
            );
        } else if separates {
            end_word(&mut word, &mut quoted, &mut words);
            if !words.is_empty() {
                simples.push(Simple {
                    words: std::mem::take(&mut words),
                    span: start..at,
                });
            }
            start = at + c.len_utf8();
        } else if c.is_whitespace() {
            end_word(&mut word, &mut quoted, &mut words);
        } else {
            word.push(c);
        }
    }
    end_word(&mut word, &mut quoted, &mut words);
    if !words.is_empty() {
        simples.push(Simple {
            words,
            span: start..command.len(),
        });
    }
    simples
}

/// The program a word runs, by its file name, in lowercase.
fn program_name(word: &str) -> String {
    word.rsplit(['/', '\\'])
        .next()
        .unwrap_or(word)
        .to_lowercase()
}

/// Programs whose string arguments are command lines themselves.
const SHELLS: [&str; 15] = [
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "eval",
    "cmd",
    "cmd.exe",
    "powershell",
    "powershell.exe",
    "pwsh",
    "pwsh.exe",
    "iex",
    "invoke-expression",
];

/// Programs that run the command their arguments spell, and shell words
/// that a command follows.
const WRAPPERS: [&str; 30] = [
    "sudo", "doas", "env", "timeout", "nice", "nohup", "time", "command", "exec", "xargs",
    "stdbuf", "ionice", "setsid", "chronic", "strace", "ltrace", "unbuffer", "watch", "eval",
    "cmd", "cmd.exe", "then", "do", "else", "if", "elif", "while", "until", "!", "busybox",
];

/// Where in a simple command's `words` the program it runs may be: the
/// first word past `NAME=value` assignments, and, after a wrapper such as
/// `sudo` or `timeout`, every word after it.
fn programs(words: &[String]) -> std::ops::Range<usize> {
    let first = words
        .iter()
        .position(|word| {
            !word
                .split_once('=')
                .is_some_and(|(name, _)| !name.is_empty() && !name.contains(['/', '\\']))
        })
        .unwrap_or(words.len());
    let wrapped = words
        .get(first)
        .is_some_and(|word| WRAPPERS.contains(&program_name(word).as_str()));
    first..if wrapped {
        words.len()
    } else {
        (first + 1).min(words.len())
    }
}

/// The `flot` commands in `command` that an agent should leave to the user,
/// as spelled by their verb: `flot init --force`, `flot trust add`, ...
/// A command run by flot counts, and so does a `flot` word anywhere after
/// a wrapper such as `timeout 5` or `sudo -u root`, and in a string a
/// shell is given to run; `echo flot gc` does not.
fn flot_verbs(command: &str, language: Shell, depth: u8, found: &mut Vec<String>) {
    for simple in simple_commands(command, language) {
        for at in programs(&simple.words) {
            let name = program_name(&simple.words[at]);
            if name == "flot" || name == "flot.exe" {
                found.extend(refused_verb(&simple.words[at + 1..]));
            }
            if depth < 4 && SHELLS.contains(&name.as_str()) {
                let inner = if ["cmd", "cmd.exe", "sh", "bash", "zsh", "dash", "ksh", "fish"]
                    .contains(&name.as_str())
                    || (name == "eval" && matches!(language, Shell::Posix))
                {
                    Shell::Posix
                } else {
                    Shell::PowerShell
                };
                for argument in &simple.words[at + 1..] {
                    if argument.contains(char::is_whitespace) {
                        flot_verbs(argument, inner, depth + 1, found);
                    }
                }
            }
        }
    }
}

/// The verb of `flot` run with `args`, when it changes whose evidence
/// counts: who is trusted and with what key, which runs this store holds,
/// the flow file, or the revision and flow proofs are held to.
fn refused_verb(args: &[String]) -> Option<String> {
    let mut words: Vec<&str> = Vec::new();
    let mut options: Vec<&str> = Vec::new();
    let mut other_flow = false;
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        at += 1;
        match arg.as_str() {
            "--" => {
                words.extend(args[at..].iter().map(String::as_str));
                break;
            }
            "-f" | "--file" => {
                other_flow = true;
                at += 1;
            }
            _ if arg.starts_with("--file=")
                || (arg.starts_with("-f") && !arg.starts_with("--")) =>
            {
                other_flow = true;
            }
            _ if arg.starts_with('-') => options.push(arg),
            _ => words.push(arg),
        }
    }
    let has = |flag: &str| {
        options
            .iter()
            .any(|option| *option == flag || option.starts_with(&format!("{flag}=")))
    };
    let verb = *words.first()?;
    let refused = match (verb, words.get(1).copied()) {
        ("init", _) if has("--force") => "init --force".to_string(),
        ("trust", Some(action @ ("add" | "remove" | "rotate")))
        | ("key", Some(action @ ("init" | "rotate"))) => format!("{verb} {action}"),
        ("sign" | "import" | "pull", _) => verb.to_string(),
        ("gc", _) if !has("--dry-run") => "gc".to_string(),
        ("run" | "verify" | "diff", _) if has("--proofs-from") => format!("{verb} --proofs-from"),
        ("run" | "verify" | "diff", _) if other_flow => format!("{verb} -f"),
        _ => return None,
    };
    Some(format!("flot {refused}"))
}

/// `word` with its `{a,b}` groups expanded as bash expands them, up to
/// [`EXPANSION_LIMIT`] words.
fn braces(word: &str) -> Vec<String> {
    let mut done = Vec::new();
    let mut pending = vec![word.to_string()];
    while let Some(word) = pending.pop() {
        match brace_group(&word) {
            Some((open, commas, close)) if done.len() + pending.len() < EXPANSION_LIMIT => {
                let mut edges = vec![open];
                edges.extend(commas);
                edges.push(close);
                for pair in edges.windows(2) {
                    pending.push(format!(
                        "{}{}{}",
                        &word[..open],
                        &word[pair[0] + 1..pair[1]],
                        &word[close + 1..]
                    ));
                }
            }
            _ => done.push(word),
        }
    }
    done
}

/// The first `{` whose matching `}` encloses a comma at its own level, not
/// a `${NAME}`: where it opens, its commas, and where it closes.
fn brace_group(word: &str) -> Option<(usize, Vec<usize>, usize)> {
    let bytes = word.as_bytes();
    for open in 0..bytes.len() {
        if bytes[open] != b'{' || (open > 0 && bytes[open - 1] == b'$') {
            continue;
        }
        let mut depth = 0;
        let mut commas = Vec::new();
        for (close, byte) in bytes.iter().enumerate().skip(open + 1) {
            match byte {
                b'{' => depth += 1,
                b'}' if depth == 0 => {
                    if !commas.is_empty() {
                        return Some((open, commas, close));
                    }
                    break;
                }
                b'}' => depth -= 1,
                b',' if depth == 0 => commas.push(close),
                _ => {}
            }
        }
    }
    None
}

/// `word` with `~`, `$HOME`, `${HOME}`, `$PWD` and `${PWD}` put in (in
/// PowerShell, also `$env:HOME` and `$env:USERPROFILE`, in any case): as
/// the path it names, and as a wildcard pattern with what was put in
/// escaped.
fn variables(word: &str, dir: &Path, language: Shell) -> (String, String) {
    let home = home_dir().map(|home| home.to_string_lossy().into_owned());
    let here = dir.to_string_lossy().into_owned();
    let mut literal = String::new();
    let mut pattern = String::new();
    let put = |value: &str, literal: &mut String, pattern: &mut String| {
        literal.push_str(value);
        pattern.push_str(&glob::Pattern::escape(value));
    };
    let mut rest = word;
    if let (Some(home), Some(after)) = (&home, word.strip_prefix('~')) {
        if after.is_empty() || after.starts_with('/') {
            put(home, &mut literal, &mut pattern);
            rest = after;
        }
    }
    while let Some(at) = rest.find('$') {
        literal.push_str(&rest[..at]);
        pattern.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (name, used) = match after.strip_prefix('{') {
            Some(inner) => match inner.find('}') {
                Some(end) => (&inner[..end], end + 2),
                None => ("", 0),
            },
            None => {
                let end = after
                    .find(|c: char| {
                        !(c.is_ascii_alphanumeric()
                            || c == '_'
                            || (c == ':' && matches!(language, Shell::PowerShell)))
                    })
                    .unwrap_or(after.len());
                (&after[..end], end)
            }
        };
        let value = match language {
            Shell::Posix => match name {
                "HOME" => home.clone(),
                "PWD" => Some(here.clone()),
                _ => None,
            },
            Shell::PowerShell => match name.to_lowercase().as_str() {
                "home" | "env:home" | "env:userprofile" => home.clone(),
                "pwd" => Some(here.clone()),
                _ => None,
            },
        };
        match value {
            Some(value) => {
                put(&value, &mut literal, &mut pattern);
                rest = &after[used..];
            }
            None => {
                literal.push('$');
                pattern.push('$');
                rest = after;
            }
        }
    }
    literal.push_str(rest);
    pattern.push_str(rest);
    (literal, pattern)
}

/// The paths a written `word` names from `dir`, expanded as the shell
/// would: braces (in a POSIX shell), then `~`, `$HOME` and `$PWD`, then
/// wildcards against the disk. A wildcard that matches nothing is the word
/// itself.
fn expand(dir: &Path, word: &str, language: Shell) -> Vec<PathBuf> {
    let words = match language {
        Shell::Posix => braces(word),
        // PowerShell on another system takes `\` as a separator too.
        Shell::PowerShell if cfg!(not(windows)) => vec![word.replace('\\', "/")],
        Shell::PowerShell => vec![word.to_string()],
    };
    let mut paths = Vec::new();
    for word in words {
        let (literal, pattern) = variables(&word, dir, language);
        if literal.contains(['*', '?', '[']) {
            let pattern = if Path::new(&literal).is_absolute() {
                pattern
            } else {
                format!(
                    "{}/{pattern}",
                    glob::Pattern::escape(&dir.to_string_lossy())
                )
            };
            let options = glob::MatchOptions {
                case_sensitive: !cfg!(any(windows, target_os = "macos")),
                require_literal_separator: true,
                require_literal_leading_dot: true,
            };
            let matched: Vec<PathBuf> = glob::glob_with(&pattern, options)
                .map(|found| found.flatten().take(EXPANSION_LIMIT).collect())
                .unwrap_or_default();
            if !matched.is_empty() {
                paths.extend(matched);
                continue;
            }
        }
        paths.push(dir.join(literal));
    }
    paths
}

/// Judge a call against `workspace`'s flow and, when `nearest`, every flow
/// from each directory it runs in and each file it changes up to the top
/// of the repository: an edit in a subproject touches that subproject's
/// proofs, and a flow file below another does not hide it.
fn judge(workspace: &Workspace, nearest: bool, call: Call) -> Result<Judged> {
    let mut guard = crate::service::Guard::new(workspace);
    let mut targets: Vec<(PathBuf, Option<String>)> = call
        .targets
        .into_iter()
        .map(|target| (target, None))
        .collect();
    let mut refused = Vec::new();
    for (dir, command, language) in &call.commands {
        let read = |text: &str| match language {
            Shell::Posix => shell::read(text),
            Shell::PowerShell => powershell::read(text),
        };
        let reading = read(command);
        let mut words = reading.writes;
        // A whole-tree git command changes what git says it would. It is
        // asked where the call runs and where a `cd` before it leads, both:
        // the `cd` may be in a subshell.
        let mut rest = command.clone();
        let mut asked = false;
        let mut here = dir.clone();
        for simple in simple_commands(command, *language) {
            let Some(at) = programs(&simple.words)
                .find(|at| matches!(program_name(&simple.words[*at]).as_str(), "git" | "git.exe"))
            else {
                here = moved(&here, &simple.words, *language);
                continue;
            };
            let mut places = vec![dir.clone()];
            if here != *dir && here.is_dir() {
                places.push(here.clone());
                if nearest {
                    guard.consult(&here);
                }
            }
            let args = unredirected(&simple.words[at + 1..]);
            let shape = format!("{} {}", simple.words[at], args.join(" "));
            let shape = shape.trim_end().to_string();
            let mut answered = false;
            for place in &places {
                let Some(answer) = crate::service::git_would_change(place, &args) else {
                    continue;
                };
                answered = true;
                match answer {
                    Ok(paths) => {
                        targets.extend(paths.into_iter().map(|path| (path, Some(shape.clone()))))
                    }
                    Err(error) => {
                        refused.push(Refused::unknowable(shape.clone(), &error));
                        break;
                    }
                }
            }
            if answered {
                asked = true;
                rest.replace_range(simple.span.clone(), &" ".repeat(simple.span.len()));
            }
        }
        if asked {
            // A directory only those commands wrote, `.` or the one `-C`
            // names, is the tree git answered for. A file they wrote, as
            // by a redirection, is judged still.
            let others = read(&rest).writes;
            words.retain(|word| {
                others.contains(word)
                    || !expand(dir, word, *language)
                        .iter()
                        .all(|path| path.is_dir())
            });
            for word in others {
                if !words.contains(&word) {
                    words.push(word);
                }
            }
        }
        for word in &words {
            targets.extend(
                expand(dir, word, *language)
                    .into_iter()
                    .map(|path| (path, None)),
            );
        }
        refused.extend(reading.remote_code.map(Refused::remote));
        let mut verbs = Vec::new();
        flot_verbs(command, *language, 0, &mut verbs);
        refused.extend(verbs.into_iter().map(Refused::verb));
        if nearest {
            guard.consult(dir);
        }
    }
    if nearest {
        guard.consult(workspace.root());
        for (target, _) in &targets {
            if let Some(parent) = target.parent() {
                guard.consult(parent);
            }
        }
    }
    // One reason per file, however many names lead to it.
    let mut guarded: Vec<Guarded> = Vec::new();
    for (target, via) in &targets {
        let Some(mut found) = guard.judge(target) else {
            continue;
        };
        if guarded.iter().any(|known| known.path == found.path) {
            continue;
        }
        if let Some(shape) = via {
            found.reason = format!("{}; `{shape}` would change it", found.reason);
        }
        guarded.push(found);
    }
    // Commands are refused where a flow is, as files are.
    if !guard.active() {
        refused.clear();
    }
    Ok(Judged {
        guarded,
        refused,
        flows: guard.workspaces().cloned().collect(),
    })
}

/// `words` without their redirections, such as `< p.diff` or `2>/dev/null`:
/// what the program itself is given.
fn unredirected(words: &[String]) -> Vec<String> {
    let mut kept = Vec::new();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        let operator = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
        if !operator.starts_with(['<', '>']) {
            kept.push(word.clone());
            continue;
        }
        // An operator alone takes the next word as its target.
        if operator.trim_start_matches(['<', '>', '&', '|']).is_empty() {
            words.next();
        }
    }
    kept
}

/// The directory a simple command of `words` leaves the shell in, from
/// `here`: where `cd`, `pushd` or `Set-Location` leads, as far as it can
/// be told; `here` for any other command.
fn moved(here: &Path, words: &[String], language: Shell) -> PathBuf {
    let Some(first) = programs(words).next() else {
        return here.to_path_buf();
    };
    if !matches!(
        program_name(&words[first]).as_str(),
        "cd" | "pushd" | "chdir" | "set-location" | "sl" | "push-location"
    ) {
        return here.to_path_buf();
    }
    let operand = words[first + 1..]
        .iter()
        .find(|word| !word.starts_with('-') || word.as_str() == "-");
    match operand.map(String::as_str) {
        None => home_dir().unwrap_or_else(|| here.to_path_buf()),
        Some("-") => here.to_path_buf(),
        Some(operand) => expand(here, operand, language)
            .into_iter()
            .next()
            .unwrap_or_else(|| here.to_path_buf()),
    }
}

/// A call the guard could not judge, as a reason to refuse it.
fn cannot_judge(error: &anyhow::Error) -> String {
    format!("this call cannot be judged ({error:#}), and a flow is here, so it is refused")
}

/// `work`'s result, with a panic in it as an error: the hook answers either
/// way.
fn unpanicked<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|panic| {
        let said = panic
            .downcast_ref::<&str>()
            .map(|said| said.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic".to_string());
        Err(anyhow::anyhow!("the guard failed: {said}"))
    })
}

/// `payload` with each lone UTF-16 surrogate escape, such as the half of an
/// emoji a cut command ends in, replaced by `\ufffd`: a JSON parser refuses
/// one, and the rest of the payload must still be judged.
fn repair_surrogates(payload: &str) -> std::borrow::Cow<'_, str> {
    let bytes = payload.as_bytes();
    // The code unit of a `\uXXXX` escape at `at`.
    let unit = |at: usize| -> Option<u16> {
        if bytes.get(at) != Some(&b'\\') || bytes.get(at + 1) != Some(&b'u') {
            return None;
        }
        let hex = payload.get(at + 2..at + 6)?;
        if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        u16::from_str_radix(hex, 16).ok()
    };
    let mut repaired = String::new();
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'\\' {
            at += 1;
            continue;
        }
        match unit(at) {
            Some(0xD800..=0xDBFF) if matches!(unit(at + 6), Some(0xDC00..=0xDFFF)) => at += 12,
            Some(0xD800..=0xDFFF) => {
                repaired.push_str(&payload[copied..at]);
                repaired.push_str("\\ufffd");
                at += 6;
                copied = at;
            }
            // Any other escape, `\\` among them, is two characters at least.
            _ => at += 2,
        }
    }
    if copied == 0 {
        return std::borrow::Cow::Borrowed(payload);
    }
    repaired.push_str(&payload[copied..]);
    std::borrow::Cow::Owned(repaired)
}

/// Answer a Claude Code `PreToolUse` payload on stdin, for the tool call
/// it names and whatever `call` already holds: a denial on stdout, noted
/// in the store for the next run to record, or nothing. The exit stays 0,
/// so the hook's `|| true` keeps a failure to run flot at all from blocking
/// every call. A call flot cannot judge (a payload that does not parse or
/// lacks what its tool always names, or a failure of the guard itself) is
/// denied where a flow is, since otherwise the agent's own input could
/// switch the guard off, and left alone elsewhere.
fn answer_hook(workspace: &Workspace, nearest: bool, cwd: &Path, call: Call) -> Result<ExitCode> {
    let mut base = cwd.to_path_buf();
    let mut tool = String::from("a tool");
    let judged = unpanicked(|| {
        let mut payload = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut payload)
            .context("cannot read the hook payload")?;
        let payload: serde_json::Value = serde_json::from_str(&repair_surrogates(&payload))
            .context("the hook payload is not JSON")?;
        if let Some(dir) = payload["cwd"].as_str() {
            base = PathBuf::from(dir);
        }
        tool = payload["tool_name"]
            .as_str()
            .context("the hook payload names no tool_name")?
            .to_string();
        anyhow::ensure!(
            payload["tool_input"].is_object(),
            "the hook payload has no tool_input"
        );
        let asked = Call::of(&base, &tool, &payload["tool_input"])?;
        let mut call = call;
        call.targets.extend(asked.targets);
        call.commands.extend(asked.commands);
        judge(workspace, nearest, call)
    });
    let (denial, reasons, flows) = match judged {
        Ok(judged) => {
            let reasons = judged.reasons();
            if reasons.is_empty() {
                return Ok(ExitCode::SUCCESS);
            }
            (judged.denial(), reasons, judged.flows)
        }
        Err(error) => {
            let mut guard = crate::service::Guard::new(workspace);
            guard.consult(&base);
            guard.consult(cwd);
            if !guard.active() {
                return Ok(ExitCode::SUCCESS);
            }
            let reason = cannot_judge(&error);
            (
                format!("flot: {reason}; ask the user to make it."),
                vec![reason],
                guard.workspaces().cloned().collect(),
            )
        }
    };
    print_json(&serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": denial,
        }
    }))?;
    // Kept for the next run to record. The denial stands even when it
    // cannot be noted. A flow reached through a link too, such as macOS's
    // /var, is noted in once.
    let mut noted = std::collections::HashSet::new();
    for flow in &flows {
        let root = std::fs::canonicalize(flow.root()).unwrap_or_else(|_| flow.root().to_path_buf());
        if noted.insert(root) {
            let _ = flow.note_refusal(&tool, &reasons);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// One tool call the hook would have refused, read back from a transcript.
#[derive(Serialize)]
struct Replayed {
    transcript: String,
    at: String,
    tool: String,
    cwd: String,
    reasons: Vec<String>,
}

#[derive(Serialize, Default)]
struct Replay {
    transcripts: usize,
    /// Calls to the tools the hook sees.
    calls: usize,
    /// Of those, calls where a flow was found.
    in_a_flow: usize,
    /// Calls whose working directory is gone, so they were not judged.
    unjudged: usize,
    refused: Vec<Replayed>,
}

/// Judge every edit, shell command and other call naming a path in Claude
/// Code transcripts (JSONL files, or directories holding them; by default
/// `~/.claude/projects`) as the hook would today, against the flows as they
/// are now.
fn replay_transcripts(paths: &[PathBuf], json: bool) -> Result<ExitCode> {
    let roots: Vec<PathBuf> = if paths.is_empty() {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .context("no home directory to find ~/.claude/projects in; name the transcripts")?;
        vec![PathBuf::from(home).join(".claude").join("projects")]
    } else {
        paths.to_vec()
    };
    let mut files = Vec::new();
    for root in &roots {
        transcripts_in(root, &mut files)
            .with_context(|| format!("cannot read {}", root.display()))?;
    }
    files.sort();
    let mut replay = Replay {
        transcripts: files.len(),
        ..Replay::default()
    };
    let mut seen = std::collections::HashSet::new();
    for file in &files {
        let text =
            std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
        for line in text.split(|&byte| byte == b'\n') {
            let Ok(entry) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            if entry["type"] != "assistant" {
                continue;
            }
            let Some(content) = entry["message"]["content"].as_array() else {
                continue;
            };
            for item in content {
                let tool = item["name"].as_str().unwrap_or_default();
                if item["type"] != "tool_use" {
                    continue;
                }
                // Another tool counts when it names a path it may change.
                let cwd = entry["cwd"].as_str().unwrap_or_default();
                let base = PathBuf::from(cwd);
                let call = Call::of(&base, tool, &item["input"]);
                if !GUARDED_TOOLS.contains(&tool) && call.as_ref().map_or(true, Call::is_empty) {
                    continue;
                }
                // A call is written again when its message is.
                if let Some(id) = item["id"].as_str() {
                    if !seen.insert(id.to_string()) {
                        continue;
                    }
                }
                replay.calls += 1;
                if cwd.is_empty() || !base.is_dir() {
                    replay.unjudged += 1;
                    continue;
                }
                let workspace = Workspace::locate(&base, None)?;
                // As the hook does, a call that cannot be judged is refused
                // where a flow is.
                let (in_a_flow, reasons) = match unpanicked(|| judge(&workspace, true, call?)) {
                    Ok(judged) => (!judged.flows.is_empty(), judged.reasons()),
                    Err(error) => {
                        let mut guard = crate::service::Guard::new(&workspace);
                        guard.consult(&base);
                        let reasons = if guard.active() {
                            vec![cannot_judge(&error)]
                        } else {
                            Vec::new()
                        };
                        (guard.active(), reasons)
                    }
                };
                if in_a_flow {
                    replay.in_a_flow += 1;
                }
                if !reasons.is_empty() {
                    replay.refused.push(Replayed {
                        transcript: file.display().to_string(),
                        at: entry["timestamp"].as_str().unwrap_or_default().to_string(),
                        tool: tool.to_string(),
                        cwd: cwd.to_string(),
                        reasons,
                    });
                }
            }
        }
    }
    if json {
        print_json(&replay)?;
    } else {
        for found in &replay.refused {
            say!(
                "refused  {}  {}  {}: {}",
                found.at,
                found.tool,
                found.cwd,
                found.reasons.join("; ")
            );
        }
        say!(
            "{} transcripts, {} edits and commands: {} where a flow is, {} refused, {} not judged because their directory is gone",
            replay.transcripts,
            replay.calls,
            replay.in_a_flow,
            replay.refused.len(),
            replay.unjudged
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// The `.jsonl` files at `path`: the file itself, or those beneath it.
fn transcripts_in(path: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !path.is_dir() {
        std::fs::metadata(path)?;
        files.push(path.to_path_buf());
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            transcripts_in(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            files.push(path);
        }
    }
    Ok(())
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
        Command::Doctor => {
            let checks = workspace.doctor();
            if cli.json {
                print_json(&checks)?;
            } else {
                for found in &checks {
                    let status = match found.status {
                        Status::Ok => "ok",
                        Status::Note => "note",
                        Status::Warn => "warn",
                        Status::Fail => "FAIL",
                    };
                    say!("{status:<5} {:<15} {}", found.check, found.detail);
                }
            }
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
                }))?;
            } else {
                for found in &judged.guarded {
                    say!("protected  {}: {}", found.path, found.reason);
                }
                for found in &refused {
                    say!("refused    {}: {}", found.path, found.reason);
                }
            }
            Ok(if judged.guarded.is_empty() && refused.is_empty() {
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
        Command::Flaky { runs } => {
            let report = workspace.flaky(*runs)?;
            if cli.json {
                print_json(&report)?;
            } else {
                for found in report.steps.iter().chain(&report.claims) {
                    match &found.claim {
                        None => say!(
                            "step `{}` (key {}) on the same inputs:",
                            found.step_id,
                            found.key
                        ),
                        Some(claim) => say!(
                            "proof of \"{}\" (step `{}`, key {}) over the same files:",
                            claim,
                            found.step_id,
                            found.key
                        ),
                    }
                    for outcome in &found.outcomes {
                        let count = outcome.runs.len();
                        say!(
                            "  {:<9} in {count} run{}, newest {}",
                            outcome.outcome,
                            if count == 1 { "" } else { "s" },
                            outcome.runs[0]
                        );
                    }
                }
                for unreadable in &report.unreadable {
                    say!("unreadable  {unreadable}");
                }
                say!(
                    "{} step(s) and {} proof(s) gave different results on the same inputs in {} run(s) read",
                    report.steps.len(),
                    report.claims.len(),
                    report.runs_read
                );
            }
            Ok(if report.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
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

fn exit_for(outcome: Outcome) -> ExitCode {
    ExitCode::from(outcome.exit_code())
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
            "{:<8} {:<20} {:<9} {}ms  claims {}v/{}u/{}r  receipt {}{}{}{}",
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
            },
            vouched_of(&report.vouched, &step.step_id)
                .map(vouched_suffix)
                .unwrap_or_default()
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
        Outcome::of_run(manifest).label().to_uppercase(),
        manifest.run_id,
        digest::short(&manifest.root_digest),
        manifest.duration_ms
    );
    render_spend(manifest);
    render_limitations(manifest);
    render_refusals(manifest);
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

/// At most this many refusals are listed one by one.
const REFUSALS_SHOWN: usize = 20;

/// What the guard refused agents while the run's evidence was being made.
fn render_refusals(manifest: &RunManifest) {
    if manifest.refusals.is_empty() {
        return;
    }
    say!("refused by the guard since the previous run:");
    for refusal in manifest.refusals.iter().take(REFUSALS_SHOWN) {
        say!("  - {}: {}", refusal.tool, refusal.reasons.join("; "));
    }
    if manifest.refusals.len() > REFUSALS_SHOWN {
        say!("  - and {} more", manifest.refusals.len() - REFUSALS_SHOWN);
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
            "  {:<20} {:<9} {}  receipt {}{}",
            step.step_id,
            step.status.label(),
            if step.cached { "cached" } else { "ran   " },
            digest::short(&step.receipt_id),
            vouched_of(&detail.vouched, &step.step_id)
                .map(vouched_suffix)
                .unwrap_or_default()
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
                if let Some(before) = &claim.before {
                    let commit = &before.commit[..before.commit.len().min(12)];
                    match before.exit_code {
                        Some(0) => say!("        before the change ({commit}): passed"),
                        Some(code) => say!("        before the change ({commit}): exited {code}"),
                        None => say!("        before the change ({commit}): stopped"),
                    }
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
        verdict_text(manifest.verdict, manifest.partial),
        manifest.claims.verified,
        manifest.claims.unverified,
        manifest.claims.refuted,
        manifest.max_parallel
    );
    render_spend(manifest);
    render_limitations(manifest);
    render_refusals(manifest);
}

/// A recorded verdict as a report states it: a run of some of the flow's
/// steps reads `partial` when they passed, and names its selection.
fn verdict_text(verdict: Verdict, partial: Option<Partial>) -> String {
    match partial {
        None => verdict.label().to_string(),
        Some(partial) if verdict == Verdict::Passed => format!(
            "partial ({} of {} steps selected, and they passed)",
            partial.selected, partial.of
        ),
        Some(partial) => format!(
            "{} ({} of {} steps selected)",
            verdict.label(),
            partial.selected,
            partial.of
        ),
    }
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
    say!("verdict {}", verdict_text(audit.verdict, audit.partial));
    if let Some(anchor) = audit.anchor() {
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
        if !anchor.unread.is_empty() {
            say!("changes no step read: {}", listed(&anchor.unread));
        }
    }
    render_authorship(audit.authorship, &audit.signers);
    if !audit.vouched.is_empty() {
        say!("reused from signed runs: {} step(s)", audit.vouched.len());
        for step in &audit.vouched {
            say!(
                "  - {}: vouched for by {}",
                step.step_id,
                named(&step.vouched_by)
            );
        }
    }
}

/// The signers a reused step rests on, as a report names them.
fn named(vouched_by: &[Voucher]) -> String {
    vouched_by
        .iter()
        .map(Voucher::label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a step line ends with when its receipt came from an imported run:
/// nothing for work this host did.
fn vouched_suffix(vouched_by: &[Voucher]) -> String {
    if vouched_by.is_empty() {
        String::new()
    } else {
        format!("  vouched for by {}", named(vouched_by))
    }
}

/// Who vouched for `step_id`, when the run reused it from an imported run.
fn vouched_of<'v>(vouched: &'v [VouchedStep], step_id: &str) -> Option<&'v [Voucher]> {
    vouched
        .iter()
        .find(|step| step.step_id == step_id)
        .map(|step| step.vouched_by.as_slice())
}

/// Up to 20 paths, and how many more there are.
fn listed(paths: &[String]) -> String {
    const SHOWN: usize = 20;
    let mut text = paths[..paths.len().min(SHOWN)].join(", ");
    if paths.len() > SHOWN {
        text.push_str(&format!(" and {} more", paths.len() - SHOWN));
    }
    text
}

/// `text` as Markdown that renders as the same text: a claim or a limitation
/// may come from an agent, and must not add links, images, markup or rows.
fn md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.'
            | '!' | '|' | '~' | '>' | '=' => {
                out.push('\\');
                out.push(character);
            }
            '<' => out.push_str("&lt;"),
            '&' => out.push_str("&amp;"),
            '\n' | '\r' => out.push(' '),
            other if other.is_control() => {}
            other => out.push(other),
        }
    }
    out
}

/// A run and the check `flot verify` made of it, for a pull request comment
/// or a CI job summary.
fn markdown_report(audit: &Audit, detail: &RunDetail) -> String {
    use std::fmt::Write;
    let manifest = &detail.manifest;
    // The same judgment as the exit code of `flot verify`.
    let headline = match (audit.result(), audit.anchor(), audit.partial) {
        (Outcome::Broken, _, _) => "evidence broken".to_string(),
        (Outcome::Unverified, Some(anchor), _)
            if !anchor.holds() && manifest.verdict == Verdict::Passed =>
        {
            format!("unverified, proofs differ from {}", md(&anchor.baseline))
        }
        (Outcome::Partial, _, Some(partial)) => format!(
            "partial, {} of {} steps selected",
            partial.selected, partial.of
        ),
        (result, _, _) => result.label().to_string(),
    };
    let mut out = String::new();
    let _ = writeln!(out, "### flot: {headline}\n");
    let _ = writeln!(
        out,
        "Run `{}` of flow {}, root `{}`{}; recorded verdict {}.\n",
        audit.run_id,
        md(&manifest.flow),
        digest::short(&audit.root_digest),
        if detail.imported { ", imported" } else { "" },
        manifest.verdict.label()
    );
    let _ = writeln!(
        out,
        "- Integrity: {}",
        if audit.intact { "intact" } else { "**broken**" }
    );
    for finding in &audit.findings {
        let _ = writeln!(out, "  - {}", md(finding));
    }
    if let Some(anchor) = audit.anchor() {
        if anchor.holds() {
            let _ = writeln!(out, "- Proofs: as at {}", md(&anchor.baseline));
        } else {
            let _ = writeln!(out, "- Proofs: **not as at {}**", md(&anchor.baseline));
            for drift in &anchor.drifted {
                let _ = writeln!(
                    out,
                    "  - `{}`: {}",
                    drift.step_id,
                    md(&drift.reasons.join(", "))
                );
            }
            if !anchor.missing.is_empty() {
                let _ = writeln!(
                    out,
                    "  - claims there that this run does not make: {}",
                    md(&anchor.missing.join(", "))
                );
            }
        }
        if !anchor.unread.is_empty() {
            let _ = writeln!(
                out,
                "- Changes no step read: {}",
                md(&listed(&anchor.unread))
            );
        }
    }
    let _ = writeln!(out, "- Authorship: {}", audit.authorship.label());
    if !audit.vouched.is_empty() {
        // Each signer once, in the order the steps name them.
        let mut signers: Vec<String> = Vec::new();
        for signer in audit.vouched.iter().flat_map(|step| &step.vouched_by) {
            let label = signer.label();
            if !signers.contains(&label) {
                signers.push(label);
            }
        }
        let _ = writeln!(
            out,
            "- Reused from signed runs: {} step(s), vouched for by {}",
            audit.vouched.len(),
            md(&signers.join(", "))
        );
    }
    let counts = manifest.claims;
    let _ = writeln!(
        out,
        "- Claims: {} verified, {} unverified, {} refuted",
        counts.verified, counts.unverified, counts.refuted
    );
    if manifest.spend.reporting_steps > 0 {
        let _ = writeln!(
            out,
            "- Spend: {}",
            crate::core::metrics::format_usd(manifest.spend.cost_micro_usd)
        );
    }
    if !manifest.refusals.is_empty() {
        let _ = writeln!(
            out,
            "- Refused by the guard since the previous run: {}",
            manifest.refusals.len()
        );
        for refusal in manifest.refusals.iter().take(REFUSALS_SHOWN) {
            let _ = writeln!(
                out,
                "  - {}: {}",
                md(&refusal.tool),
                md(&refusal.reasons.join("; "))
            );
        }
        if manifest.refusals.len() > REFUSALS_SHOWN {
            let _ = writeln!(
                out,
                "  - and {} more",
                manifest.refusals.len() - REFUSALS_SHOWN
            );
        }
    }
    let _ = writeln!(out, "\n| Step | Status | Claim | |\n|---|---|---|---|");
    for step in &manifest.steps {
        let status = format!(
            "{}{}{}",
            step.status.label(),
            if step.cached { ", reused" } else { "" },
            vouched_of(&audit.vouched, &step.step_id)
                .map(|vouched_by| format!(", vouched for by {}", md(&named(vouched_by))))
                .unwrap_or_default()
        );
        let claims = detail.step_claims.get(&step.step_id);
        match claims.filter(|claims| !claims.is_empty()) {
            None => {
                let _ = writeln!(out, "| `{}` | {status} | | |", step.step_id);
            }
            Some(claims) => {
                for (index, claim) in claims.iter().enumerate() {
                    let step_cell = if index == 0 {
                        format!("`{}`", step.step_id)
                    } else {
                        String::new()
                    };
                    let status_cell = if index == 0 { status.as_str() } else { "" };
                    let text = match &claim.reason {
                        Some(reason) => format!("{} ({})", md(&claim.text), md(reason)),
                        None => md(&claim.text),
                    };
                    let _ = writeln!(
                        out,
                        "| {step_cell} | {status_cell} | {text} | {} |",
                        claim.status.label()
                    );
                }
            }
        }
    }
    if !manifest.limitations.is_empty() {
        let _ = writeln!(out, "\n<details><summary>Limitations</summary>\n");
        for limitation in &manifest.limitations {
            let _ = writeln!(out, "- {}", md(limitation));
        }
        let _ = writeln!(out, "\n</details>");
    }
    out
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
        match (&signer.problem, &signer.trusted_as, signer.this_host) {
            (Some(problem), _, _) => say!("  x {}  {problem}", attest::fingerprint(&signer.key)),
            (None, Some(name), _) => say!(
                "  + {}  trusted as {name}",
                attest::fingerprint(&signer.key)
            ),
            (None, None, true) => say!(
                "  ~ {}  this host's key, not on the trust list",
                attest::fingerprint(&signer.key)
            ),
            (None, None, false) => say!(
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
    use super::{braces, flot_verbs, reads_only, repair_surrogates, unpanicked, words_of, Shell};

    #[test]
    fn a_lone_surrogate_escape_is_replaced_and_a_pair_is_kept() {
        assert_eq!(
            repair_surrogates(r#"{"a":"x\ud83d"}"#),
            r#"{"a":"x\ufffd"}"#
        );
        assert_eq!(
            repair_surrogates(r#""\ude00 \ud83d\ude00""#),
            r#""\ufffd \ud83d\ude00""#
        );
        assert_eq!(repair_surrogates(r#""\\ud83d""#), r#""\\ud83d""#);
        assert_eq!(repair_surrogates(r#""\ud83d"#), r#""\ufffd"#);
        let value: serde_json::Value =
            serde_json::from_str(&repair_surrogates(r#"{"c":"rm check.sh \ud83d"}"#)).unwrap();
        assert_eq!(value["c"], "rm check.sh \u{fffd}");
    }

    #[test]
    fn a_panic_in_the_guard_is_an_error() {
        let failed = unpanicked(|| -> anyhow::Result<()> { panic!("boom") });
        assert!(format!("{:#}", failed.unwrap_err()).contains("boom"));
        assert_eq!(unpanicked(|| Ok(1)).unwrap(), 1);
    }

    #[test]
    fn braces_expand_as_bash_expands_them() {
        let mut found = braces("{check,tests/accept}.sh");
        found.sort();
        assert_eq!(found, ["check.sh", "tests/accept.sh"]);
        let mut found = braces("a{b,{c,d}}e");
        found.sort();
        assert_eq!(found, ["abe", "ace", "ade"]);
        assert_eq!(braces("${HOME}/x"), ["${HOME}/x"]);
        assert_eq!(braces("{solo}.sh"), ["{solo}.sh"]);
        assert!(braces(&"{a,b}".repeat(20)).len() <= super::EXPANSION_LIMIT + 2);
    }

    #[test]
    fn a_tool_is_read_only_by_its_name_unless_it_names_a_change() {
        assert_eq!(
            words_of("mcp__fs__read_text_file"),
            ["mcp", "fs", "read", "text", "file"]
        );
        assert_eq!(words_of("NotebookEdit"), ["notebook", "edit"]);
        for tool in [
            "Read",
            "LS",
            "Glob",
            "Grep",
            "WebFetch",
            "mcp__fs__list_directory",
        ] {
            assert!(reads_only(tool), "{tool}");
        }
        for tool in [
            "Write",
            "mcp__fs__write_file",
            "mcp__fs__move_file",
            "search_replace",
            "readAndWrite",
            "download_file",
            "TodoWrite",
        ] {
            assert!(!reads_only(tool), "{tool}");
        }
    }

    #[test]
    fn a_flot_command_is_found_where_it_runs_and_not_where_it_is_text() {
        let verbs = |command: &str| {
            let mut found = Vec::new();
            flot_verbs(command, Shell::Posix, 0, &mut found);
            found
        };
        for (command, verb) in [
            ("flot gc", "flot gc"),
            ("timeout 5 flot gc", "flot gc"),
            ("FOO=1 ./target/debug/flot sign r", "flot sign"),
            ("bash -c 'flot trust add a b'", "flot trust add"),
            ("true && flot -f other.toml run", "flot run -f"),
            ("flot run --proofs-from=main", "flot run --proofs-from"),
            ("flot init --force", "flot init --force"),
        ] {
            assert_eq!(verbs(command), [verb], "{command}");
        }
        for command in [
            "echo flot gc",
            "git commit -m 'flot gc'",
            "flot gc --dry-run",
            "flot verify",
            "flot trust list",
            "flot init",
        ] {
            assert!(verbs(command).is_empty(), "{command}");
        }
    }

    #[test]
    fn control_sequences_and_bidi_overrides_are_made_visible() {
        assert_eq!(terminal_safe("plain\ttext\nok"), "plain\ttext\nok");
        assert_eq!(terminal_safe("\u{1b}[2Jgone"), "\\u{1b}[2Jgone");
        assert_eq!(terminal_safe("over\rwrite"), "over\\u{d}write");
        assert_eq!(terminal_safe("a\u{202E}b"), "a\\u{202e}b");
        assert_eq!(terminal_safe("한글 ✓"), "한글 ✓");
    }
}
