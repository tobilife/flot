//! The guard as the CLI runs it: a tool call read from a Claude Code hook
//! payload or the command line, judged against every flow it may touch,
//! and the hook's answer.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};

use crate::core::{powershell, shell};
use crate::service::{Guarded, Workspace};

use super::render::print_json;

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
pub(super) const GUARDED_TOOLS: [&str; 6] = [
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
pub(super) enum Shell {
    Posix,
    PowerShell,
}

/// What one tool call would change: files it names, and command lines with
/// the directory they run in and their shell; and the files it only reads,
/// which are judged for the secrets among them.
#[derive(Default)]
pub(super) struct Call {
    pub(super) targets: Vec<PathBuf>,
    pub(super) commands: Vec<(PathBuf, String, Shell)>,
    pub(super) named: Vec<PathBuf>,
}

impl Call {
    /// A `tool`'s input as Claude Code gives it, run from `base`. An edit or
    /// a shell call without the file or command line its tool always names
    /// cannot be judged, which is an error.
    pub(super) fn of(base: &Path, tool: &str, input: &serde_json::Value) -> Result<Call> {
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
            _ if reads_only(tool) => named_paths(input, 0, &mut |path| {
                call.named.push(base.join(tilde(path)));
            }),
            _ => named_paths(input, 0, &mut |path| {
                call.targets.push(base.join(tilde(path)));
            }),
        }
        Ok(call)
    }

    /// Whether it changes nothing it names: it names no file it may change
    /// and runs no command, though it may read.
    pub(super) fn is_empty(&self) -> bool {
        self.targets.is_empty() && self.commands.is_empty()
    }
}

/// A command the guard refuses whatever files it names.
pub(super) struct Refused {
    /// The command as spelled, such as `curl | sh` or `flot gc`.
    pub(super) shape: String,
    /// Why, said of the command.
    pub(super) why: String,
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

    /// A command whose code the guard cannot read, and why.
    fn unread(shape: String, why: String) -> Self {
        Refused {
            said: format!("`{shape}` is refused: {why}"),
            why,
            instead: ". Run what it would run as a command of its own, or ask the user",
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
pub(super) struct Judged {
    pub(super) guarded: Vec<Guarded>,
    /// Downloaded code piped into an interpreter, `flot` commands left to
    /// the user, git commands whose files git cannot name, code the guard
    /// cannot read.
    pub(super) refused: Vec<Refused>,
    /// The signing key and what goes with it, named by the call, flow or
    /// not.
    pub(super) secrets: Vec<Guarded>,
    /// The flows found, each with its flow file.
    pub(super) flows: Vec<Workspace>,
}

impl Judged {
    /// Each refusal in a sentence: the protected files first, then the
    /// refused commands, then the secrets.
    pub(super) fn reasons(&self) -> Vec<String> {
        self.guarded
            .iter()
            .map(|found| format!("{} is protected: {}", found.path, found.reason))
            .chain(self.refused.iter().map(|refused| refused.said.clone()))
            .chain(
                self.secrets
                    .iter()
                    .map(|found| format!("{} must not be read: {}", found.path, found.reason)),
            )
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
        if !self.secrets.is_empty() {
            said.push(format!(
                "{}. `flot key show` prints the public key; ask the user for anything that needs the key itself",
                reasons[reasons.len() - self.secrets.len()..].join("; ")
            ));
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
    let mut chars = command.char_indices().peekable();
    let end_word = |word: &mut String, quoted: &mut bool, words: &mut Vec<String>| {
        if !word.is_empty() || *quoted {
            words.push(std::mem::take(word));
        }
        *quoted = false;
    };
    // The character before, outside quotes: a `#` after a blank or an
    // operator starts a comment.
    let mut before: Option<char> = None;
    while let Some((at, c)) = chars.next() {
        let separates = matches!(c, ';' | '&' | '|' | '\n' | '(' | ')' | '{' | '}')
            || (c == '`' && matches!(language, Shell::Posix));
        let comment = c == '#'
            && word.is_empty()
            && !quoted
            && before.is_none_or(|before| before.is_whitespace() || ";&|()".contains(before));
        before = Some(c);
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
        } else if comment {
            // A comment runs to the end of the line: `git reset --hard #
            // why` gives git no `#`.
            while chars.next_if(|(_, next)| *next != '\n').is_some() {}
        } else if c == '$' && chars.peek().is_some_and(|(_, next)| *next == '{') {
            // `${..}` is one word, not a brace group: `${#x}` is no comment.
            word.push(c);
            let mut depth = 0;
            for (_, c) in chars.by_ref() {
                word.push(c);
                match c {
                    '{' => depth += 1,
                    '}' if depth == 1 => break,
                    '}' => depth -= 1,
                    _ => {}
                }
            }
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

/// The value of the environment variable `name`, which the shell the
/// agent runs is given too; `None` for one not set or not a name.
fn environment(name: &str) -> Option<String> {
    let mut chars = name.chars();
    let is_name = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !is_name {
        return None;
    }
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

/// `word` with `~`, `$HOME`, `$PWD` and the other environment variables
/// put in, `${NAME}` too (in PowerShell, `$HOME`, `$PWD` and `$env:NAME`
/// in any case, `$env:HOME` and `$env:USERPROFILE` as the home directory
/// where they are not set): as the path it names, and as a wildcard
/// pattern with what was put in escaped.
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
                _ => environment(name),
            },
            Shell::PowerShell => match name.to_lowercase().as_str() {
                "home" => home.clone(),
                "pwd" => Some(here.clone()),
                lower => lower.strip_prefix("env:").and_then(|_| {
                    environment(&name[4..]).or_else(|| {
                        matches!(lower, "env:home" | "env:userprofile")
                            .then(|| home.clone())
                            .flatten()
                    })
                }),
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

/// The words a written `word` stands for before variables are put in:
/// its braces expanded in a POSIX shell; in PowerShell on another system,
/// with `\` as a separator.
fn alternatives(word: &str, language: Shell) -> Vec<String> {
    match language {
        Shell::Posix => braces(word),
        Shell::PowerShell if cfg!(not(windows)) => vec![word.replace('\\', "/")],
        Shell::PowerShell => vec![word.to_string()],
    }
}

/// A relative wildcard `pattern` as one from `dir`.
fn anchored(dir: &Path, pattern: &str) -> String {
    format!(
        "{}/{pattern}",
        glob::Pattern::escape(&dir.to_string_lossy())
    )
}

/// How a shell matches a wildcard against paths.
fn wildcard_match() -> glob::MatchOptions {
    glob::MatchOptions {
        case_sensitive: !cfg!(any(windows, target_os = "macos")),
        require_literal_separator: true,
        require_literal_leading_dot: true,
    }
}

/// The paths a written `word` names from `dir`, expanded as the shell
/// would: braces (in a POSIX shell), then `~` and variables, then
/// wildcards against the disk. A wildcard that matches nothing is the word
/// itself.
fn expand(dir: &Path, word: &str, language: Shell) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for word in alternatives(word, language) {
        let (literal, pattern) = variables(&word, dir, language);
        if literal.contains(['*', '?', '[']) {
            let pattern = if Path::new(&literal).is_absolute() {
                pattern
            } else {
                anchored(dir, &pattern)
            };
            let matched: Vec<PathBuf> = glob::glob_with(&pattern, wildcard_match())
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

/// How many scripts the guard reads for one call; a call that runs more
/// cannot be judged.
const SCRIPT_LIMIT: usize = 64;

/// The largest script the guard reads, in bytes.
const SCRIPT_BYTES: u64 = 1 << 20;

/// Code the guard reads: where it runs, in what language, how many times
/// the code that runs it was handed over as a string, and, for a script
/// read from the disk, its `$0` and `$BASH_SOURCE`.
struct Pending {
    dir: PathBuf,
    text: String,
    language: Shell,
    depth: usize,
    zero: Option<String>,
    source: Option<String>,
}

/// Judge a call against `workspace`'s flow and, when `nearest`, every flow
/// from each directory it runs in and each file it changes up to the top
/// of the repository: an edit in a subproject touches that subproject's
/// proofs, and a flow file below another does not hide it. The scripts a
/// command runs are read and judged as it is, and the code it hands an
/// interpreter for what it names.
pub(super) fn judge(workspace: &Workspace, nearest: bool, call: Call) -> Result<Judged> {
    let mut guard = crate::service::Guard::new(workspace);
    if nearest {
        guard.consult(workspace.root());
    }
    let mut secrets: Vec<Guarded> = Vec::new();
    for path in call.targets.iter().chain(&call.named) {
        noted(&mut secrets, guard.secret(path));
    }
    let mut targets: Vec<(PathBuf, Option<String>)> = call
        .targets
        .into_iter()
        .map(|target| (target, None))
        .collect();
    let mut refused = Vec::new();
    let mut pending: std::collections::VecDeque<Pending> = call
        .commands
        .into_iter()
        .map(|(dir, text, language)| Pending {
            dir,
            text,
            language,
            depth: 0,
            zero: None,
            source: None,
        })
        .collect();
    let mut scripts_read = std::collections::HashSet::new();
    // The scripts the call writes text into that it spells, and that text.
    let mut written: Vec<(PathBuf, String)> = Vec::new();
    while let Some(Pending {
        dir,
        text,
        language,
        depth,
        zero,
        source,
    }) = pending.pop_front()
    {
        if nearest {
            guard.consult(&dir);
        }
        let origin = shell::Origin {
            dir: Some(dir.to_string_lossy().into_owned()),
            zero: zero.clone(),
            source,
        };
        let read = |text: &str, depth: usize| match language {
            Shell::Posix => shell::read_at(text, depth, &origin),
            Shell::PowerShell => powershell::read_at(text, depth),
        };
        let reading = read(&text, depth);
        let mut words = reading.writes;
        // A whole-tree git command changes what git says it would. It is
        // asked where the call runs and where a `cd` before it leads, both:
        // the `cd` may be in a subshell.
        let mut rest = text.clone();
        let mut asked = false;
        let mut here = dir.clone();
        for simple in simple_commands(&text, language) {
            let Some(at) = programs(&simple.words)
                .find(|at| matches!(program_name(&simple.words[*at]).as_str(), "git" | "git.exe"))
            else {
                here = moved(&here, &simple.words, language);
                continue;
            };
            let mut places = vec![dir.clone()];
            if here != dir && here.is_dir() {
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
                    Ok(paths) => targets.extend(
                        paths
                            .into_iter()
                            .map(|path| (path, Some(format!("`{shape}` would change it")))),
                    ),
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
            let others = read(&rest, depth).writes;
            words.retain(|word| {
                others.contains(word)
                    || !expand(&dir, word, language)
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
            if word.contains([shell::BUILT, shell::SPELLED]) {
                // Part of the name is what a command prints when it runs:
                // any guarded place it may spell.
                let via = format!(
                    "`{}` may name it: part of that name is built when the command runs",
                    shell::shown(word)
                );
                for pattern in built_patterns(&dir, word, language) {
                    targets.extend(
                        guard
                            .matching(&pattern)
                            .into_iter()
                            .map(|path| (path, Some(via.clone()))),
                    );
                }
            } else {
                targets.extend(
                    expand(&dir, word, language)
                        .into_iter()
                        .map(|path| (path, None)),
                );
            }
        }
        for word in &reading.named {
            noted(&mut secrets, secret_named(&guard, &dir, word, language));
        }
        for code in &reading.code {
            let at = located_dir(&dir, &code.dir, language);
            let via = format!(
                "{} code names it, and flot cannot tell whether that code writes it",
                code.program
            );
            for path in guard.named_in(&code.text, &at) {
                match guard.secret(&path) {
                    Some(secret) => noted(&mut secrets, Some(secret)),
                    None => targets.push((path, Some(via.clone()))),
                }
            }
        }
        for script in reading.scripts {
            let Some(path) = expand(&dir, &script.path, language).into_iter().next() else {
                continue;
            };
            let runs_in = located_dir(&dir, &script.dir, language);
            let shape = shell::shown(&script.path);
            if !scripts_read.insert((
                path.clone(),
                runs_in.clone(),
                format!("{:?}", script.runner),
            )) {
                continue;
            }
            if scripts_read.len() > SCRIPT_LIMIT {
                refused.push(Refused::unread(
                    shape,
                    format!("the call runs more than {SCRIPT_LIMIT} scripts, so flot cannot read all it runs"),
                ));
                break;
            }
            // What the call writes into it before running it is read as
            // well as what is on the disk.
            if let Some(text) = &script.text {
                written.push((path.clone(), text.clone()));
            }
            let mut texts: Vec<String> = written
                .iter()
                .filter(|(known, _)| *known == path)
                .map(|(_, text)| text.clone())
                .collect();
            match script_text(&path) {
                Ok(Some(text)) => texts.push(text),
                Ok(None) => {}
                Err(error) => {
                    refused.push(Refused::unread(
                        shape,
                        format!("{error:#}, so flot cannot read what it runs"),
                    ));
                    continue;
                }
            }
            if texts.is_empty() {
                continue;
            }
            if nearest {
                guard.consult(&runs_in);
            }
            let depth = script.depth + 1;
            // In a script a shell runs, `$0` is its name as the command
            // gave it; one `.` or `source` reads keeps the caller's, from
            // where the caller started.
            let named = Some(script.named.clone());
            let zero = if script.sourced {
                zero.as_ref()
                    .map(|zero| dir.join(zero).to_string_lossy().into_owned())
            } else {
                named.clone()
            };
            for text in texts {
                let (text, language, zero, source) = match &script.runner {
                    shell::Runner::Shell => (text, Shell::Posix, zero.clone(), named.clone()),
                    shell::Runner::PowerShell => (text, Shell::PowerShell, None, None),
                    shell::Runner::Interpreter(program) => {
                        // A script the flow guards is the user's check, as
                        // the proof runs it: the agent cannot have changed
                        // it.
                        if guard.judge(&path).is_some() {
                            continue;
                        }
                        let via = format!(
                            "the {program} script {shape} names it, and flot cannot tell whether that script writes it"
                        );
                        for named in guard.named_in(&text, &runs_in) {
                            match guard.secret(&named) {
                                Some(secret) => noted(&mut secrets, Some(secret)),
                                None => targets.push((named, Some(via.clone()))),
                            }
                        }
                        continue;
                    }
                    // A program's `#!` line names what runs it, which reads
                    // the script again; a text file without one is run by
                    // the shell.
                    shell::Runner::Program => match shebang(&text) {
                        Some(line) => {
                            let named = path.strip_prefix(&runs_in).unwrap_or(&path);
                            (
                                format!("{line} {}", quoted(named)),
                                Shell::Posix,
                                None,
                                None,
                            )
                        }
                        None if path
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("ps1")) =>
                        {
                            (text, Shell::PowerShell, None, None)
                        }
                        None => (text, Shell::Posix, named.clone(), named.clone()),
                    },
                };
                pending.push_back(Pending {
                    dir: runs_in.clone(),
                    text,
                    language,
                    depth,
                    zero,
                    source,
                });
            }
        }
        refused.extend(
            reading
                .unread
                .into_iter()
                .map(|unread| Refused::unread(unread.shape, unread.why)),
        );
        refused.extend(reading.remote_code.map(Refused::remote));
        let mut verbs = Vec::new();
        flot_verbs(&text, language, 0, &mut verbs);
        refused.extend(verbs.into_iter().map(Refused::verb));
    }
    if nearest {
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
        if let Some(via) = via {
            found.reason = format!("{}; {via}", found.reason);
        }
        guarded.push(found);
    }
    // Commands are refused where a flow is, as files are. A secret is
    // refused anywhere.
    if !guard.active() {
        refused.clear();
    }
    Ok(Judged {
        guarded,
        refused,
        secrets,
        flows: guard.workspaces().cloned().collect(),
    })
}

/// Adds `found` to `secrets`, once for each secret.
fn noted(secrets: &mut Vec<Guarded>, found: Option<Guarded>) {
    if let Some(found) = found {
        if !secrets.iter().any(|known| known.path == found.path) {
            secrets.push(found);
        }
    }
}

/// The directory `dir`, as the reader locates where code runs, names from
/// `base`; `base` for an empty one.
fn located_dir(base: &Path, dir: &str, language: Shell) -> PathBuf {
    if dir.is_empty() {
        return base.to_path_buf();
    }
    expand(base, dir, language)
        .into_iter()
        .next()
        .unwrap_or_else(|| base.to_path_buf())
}

/// The text of the script at `path`, up to [`SCRIPT_BYTES`]: `None` when it
/// is no file, or no text (a NUL in its first 8 KiB, or not UTF-8), as a
/// compiled program is; an error when it is larger or cannot be read.
fn script_text(path: &Path) -> Result<Option<String>> {
    use std::io::Read;
    if !path.is_file() {
        return Ok(None);
    }
    let mut file =
        std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(8192)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read {}", path.display()))?;
    if bytes.contains(&0) {
        return Ok(None);
    }
    file.take(SCRIPT_BYTES + 1 - bytes.len() as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read {}", path.display()))?;
    if bytes.len() as u64 > SCRIPT_BYTES {
        anyhow::bail!("{} is larger than 1 MiB", path.display());
    }
    if bytes.contains(&0) {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok())
}

/// What a script's `#!` line runs it with, as `/usr/bin/env python3`.
fn shebang(text: &str) -> Option<&str> {
    let line = text.strip_prefix("#!")?.lines().next()?.trim();
    (!line.is_empty()).then_some(line)
}

/// `path` as one word of a POSIX command line.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// The wildcard patterns a written `word` that holds what a command prints
/// when it runs may match, that part as `*`, from `dir`: a word that
/// starts with it may be any path. What another program prints is a value
/// flot cannot know, as a variable it does not know is: a word that is
/// nothing else, or that with only separators and wildcards, names
/// nothing; one with a part around it is matched by that part, as
/// `$(..)/check.sh`. What a program that makes text prints is such a part:
/// it may spell any name.
fn built_patterns(dir: &Path, word: &str, language: Shell) -> Vec<String> {
    alternatives(word, language)
        .into_iter()
        .filter(|word| spells_a_part(word))
        .map(|word| {
            let (literal, pattern) = variables(&word, dir, language);
            let pattern = pattern.replace([shell::BUILT, shell::SPELLED], "*");
            if literal.starts_with([shell::BUILT, shell::SPELLED])
                || Path::new(&literal).is_absolute()
            {
                pattern
            } else {
                anchored(dir, &pattern)
            }
        })
        .collect()
}

/// Whether `word`, past what programs that do not make text print in it,
/// spells a part of a name: anything but separators, `.`, `..` and
/// wildcards.
fn spells_a_part(word: &str) -> bool {
    word.split(['/', '\\', shell::BUILT]).any(|part| {
        !matches!(part, "" | "." | "..") && !part.chars().all(|c| matches!(c, '*' | '?'))
    })
}

/// The secret a `word` a command is given in `dir` may name: the path it
/// names, or, with a wildcard, any secret it matches.
fn secret_named(
    guard: &crate::service::Guard,
    dir: &Path,
    word: &str,
    language: Shell,
) -> Option<Guarded> {
    for word in alternatives(word, language) {
        let (literal, pattern) = variables(&word, dir, language);
        if literal.contains(['*', '?', '[']) {
            let pattern = if Path::new(&literal).is_absolute() {
                pattern
            } else {
                anchored(dir, &pattern)
            };
            let Ok(pattern) = glob::Pattern::new(&pattern) else {
                continue;
            };
            let found = guard
                .secrets()
                .into_iter()
                .filter(|secret| pattern.matches_path_with(secret, wildcard_match()))
                .find_map(|secret| guard.secret(&secret));
            if found.is_some() {
                return found;
            }
        } else if let Some(found) = guard.secret(&dir.join(literal)) {
            return Some(found);
        }
    }
    None
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
pub(super) fn cannot_judge(error: &anyhow::Error) -> String {
    format!("this call cannot be judged ({error:#}), and a flow is here, so it is refused")
}

/// `work`'s result, with a panic in it as an error: the hook answers either
/// way.
pub(super) fn unpanicked<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
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
pub(super) fn answer_hook(
    workspace: &Workspace,
    nearest: bool,
    cwd: &Path,
    call: Call,
) -> Result<ExitCode> {
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
        call.named.extend(asked.named);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
