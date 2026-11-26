//! The judge: what one tool call would change, read from the command
//! lines it runs and the scripts and code they hand on, held against
//! every flow it may touch; and the answer the guard gives, refusing
//! where a flow is what it cannot judge.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::core::{powershell, shell};
use crate::service::Workspace;

use super::expand::{alternatives, anchored, expand, variables, wildcard_match};
use super::git::git_would_change;
use super::rules::Guard;
use super::verbs::{flot_verb, program_name, runs};
use super::Guarded;

/// Why downloaded code piped into an interpreter is refused.
const REMOTE: &str = "it runs code fetched from the network, which no step or receipt records";

/// What a `flot` command the guard leaves to the user does.
const VERB: &str = "changes whose evidence counts";

/// The language a command line is read in.
#[derive(Clone, Copy)]
pub enum Shell {
    Posix,
    PowerShell,
}

/// What one tool call would change: files it names, and command lines with
/// the directory they run in and their shell; and the files it only reads,
/// which are judged for the secrets among them.
#[derive(Default)]
pub struct Call {
    pub targets: Vec<PathBuf>,
    pub commands: Vec<(PathBuf, String, Shell)>,
    pub named: Vec<PathBuf>,
}

impl Call {
    /// Whether it changes nothing it names: it names no file it may change
    /// and runs no command, though it may read.
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty() && self.commands.is_empty()
    }
}

/// A command the guard refuses whatever files it names.
pub struct Refused {
    /// The command as spelled, such as `curl | sh` or `flot gc`.
    pub shape: String,
    /// Why, said of the command.
    pub why: String,
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
pub struct Judged {
    pub guarded: Vec<Guarded>,
    /// Downloaded code piped into an interpreter, `flot` commands left to
    /// the user, git commands whose files git cannot name, code the guard
    /// cannot read.
    pub refused: Vec<Refused>,
    /// The signing key and what goes with it, named by the call, flow or
    /// not.
    pub secrets: Vec<Guarded>,
    /// The flows found, each with its flow file.
    flows: Vec<Workspace>,
}

impl Judged {
    /// Each refusal in a sentence: the protected files first, then the
    /// refused commands, then the secrets.
    fn reasons(&self) -> Vec<String> {
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
pub fn judge(workspace: &Workspace, nearest: bool, call: Call) -> Result<Judged> {
    let mut guard = Guard::new(workspace);
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
    // The git commands asked about, each where it was asked, and whether
    // git answered: one answer each.
    let mut asked: std::collections::HashMap<(PathBuf, Vec<String>), bool> =
        std::collections::HashMap::new();
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
        let reading = match language {
            Shell::Posix => shell::read_at(&text, depth, &origin),
            Shell::PowerShell => powershell::read_at(&text, depth),
        };
        let mut words = reading.writes;
        // A whole-tree git command changes what git says it would. It is
        // asked where the call runs and where the `cd`s before it lead,
        // both: the `cd` may be in a subshell.
        let mut answered = vec![false; reading.commands.len()];
        for (command, answered) in reading.commands.iter().zip(&mut answered) {
            // Another host's git changes that host's repository.
            if command.remote {
                continue;
            }
            for (program, args) in runs(command).filter_map(<[String]>::split_first) {
                if !matches!(program_name(program).as_str(), "git" | "git.exe") {
                    continue;
                }
                let here = located_dir(&dir, &command.dir, language);
                let mut places = vec![dir.clone()];
                if here != dir && here.is_dir() {
                    places.push(here);
                    if nearest {
                        guard.consult(&places[1]);
                    }
                }
                // What is built when the line runs is no revision or path
                // git knows: it is given as shown.
                let args: Vec<String> = args.iter().map(|arg| shell::shown(arg)).collect();
                let shape = shell::shown(format!("{program} {}", args.join(" ")).trim_end());
                for place in places {
                    let question = (place, args.clone());
                    if let Some(known) = asked.get(&question) {
                        *answered |= *known;
                        continue;
                    }
                    let answer = git_would_change(&question.0, &args);
                    asked.insert(question, answer.is_some());
                    let Some(answer) = answer else {
                        continue;
                    };
                    *answered = true;
                    match answer {
                        Ok(paths) => targets.extend(
                            paths
                                .into_iter()
                                .map(|path| (path, Some(format!("`{shape}` would change it")))),
                        ),
                        Err(error) => {
                            refuse(&mut refused, Refused::unknowable(shape.clone(), &error));
                            break;
                        }
                    }
                }
            }
        }
        if answered.contains(&true) {
            // A directory only those commands write, `.` or the one `-C`
            // names, is the tree git answered for. A file they write, as by
            // a redirection, is judged still, and so is a directory another
            // command writes, the shell that runs them among them.
            let mut tree = std::collections::HashSet::new();
            let mut others = std::collections::HashSet::new();
            for (command, answered) in reading.commands.iter().zip(&answered) {
                if *answered {
                    tree.extend(&command.writes);
                } else {
                    others.extend(&command.writes);
                }
            }
            words.retain(|word| {
                !tree.contains(word)
                    || others.contains(word)
                    || !expand(&dir, word, language)
                        .iter()
                        .all(|path| path.is_dir())
            });
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
        for verb in reading.commands.iter().flat_map(runs).filter_map(flot_verb) {
            refuse(&mut refused, Refused::verb(verb));
        }
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

/// Adds `found` to `refused`, once: a command read in each directory it
/// may run in is refused once.
fn refuse(refused: &mut Vec<Refused>, found: Refused) {
    if !refused.iter().any(|known| known.said == found.said) {
        refused.push(found);
    }
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
fn secret_named(guard: &Guard, dir: &Path, word: &str, language: Shell) -> Option<Guarded> {
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

/// A call the guard could not judge, as a reason to refuse it.
fn cannot_judge(error: &anyhow::Error) -> String {
    format!("this call cannot be judged ({error:#}), and a flow is here, so it is refused")
}

/// What the guard answers a call: why it is refused, none when it is not;
/// the denial Claude Code shows the agent; and the flows to note it in.
#[derive(Default)]
pub struct Answer {
    pub reasons: Vec<String>,
    pub denial: String,
    pub flows: Vec<Workspace>,
}

/// The answer to a call `judged` as it was. A call flot cannot judge (an
/// input that lacks what its tool always names, or a failure of the guard
/// itself) is refused where a flow is, found from `workspace` and each of
/// `places`, since otherwise the agent's own input could switch the guard
/// off; elsewhere it is left alone.
pub fn answer(workspace: &Workspace, places: &[&Path], judged: Result<Judged>) -> Answer {
    match judged {
        Ok(judged) => Answer {
            reasons: judged.reasons(),
            denial: judged.denial(),
            flows: judged.flows,
        },
        Err(error) => {
            let mut guard = Guard::new(workspace);
            for place in places {
                guard.consult(place);
            }
            if !guard.active() {
                return Answer::default();
            }
            let reason = cannot_judge(&error);
            Answer {
                denial: format!("flot: {reason}; ask the user to make it."),
                reasons: vec![reason],
                flows: guard.workspaces().cloned().collect(),
            }
        }
    }
}

impl Answer {
    /// Note the refusal in each flow's store, for the next run to record.
    /// The denial stands even when it cannot be noted. A flow reached
    /// through a link too, such as macOS's /var, is noted in once.
    pub fn note(&self, tool: &str) {
        let mut noted = std::collections::HashSet::new();
        for flow in &self.flows {
            let root =
                std::fs::canonicalize(flow.root()).unwrap_or_else(|_| flow.root().to_path_buf());
            if noted.insert(root) {
                let _ = flow.note_refusal(tool, &self.reasons);
            }
        }
    }
}
