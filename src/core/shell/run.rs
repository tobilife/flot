//! What one simple command changes and runs.

use crate::core::reading::{
    merge, overflowed, ran, told, Code, Command, Reading, Runner, Script, Unread, BUILT, DEPTH,
    RUNS, SPELLED,
};

use super::command::{command, parse, program};
use super::read::read_in;
use super::tables::{language, Language, SHELLS, TEXT_ONLY};
use super::vars::Variables;
use super::writers::written;
use super::Origin;

/// Why a command whose code is built when the line runs is not seen.
const BUILT_CODE: &str =
    "what it runs is built when the command runs, so flot cannot read it before it runs";

/// A command whose code is built when the line runs, as `program $(..)`.
fn built(program: &str) -> Reading {
    Reading {
        unread: vec![Unread {
            shape: format!("{program} $(..)"),
            why: BUILT_CODE.to_string(),
        }],
        ..Reading::default()
    }
}

/// What `program` runs of `code`, a string it reads as commands: the code
/// read again, `depth` strings deep, with `vars` the variables it sees,
/// where `origin` says. A
/// part of it some other program prints is as unknown as a variable that
/// is not set; code spelled by a program that makes text is not read.
fn code_of(program: &str, code: &str, depth: usize, vars: &Variables, origin: &Origin) -> Reading {
    if code.contains(SPELLED) {
        built(program)
    } else {
        read_in(code, depth + 1, vars, origin)
    }
}

/// The script `program` runs at `path`, for the caller to read. A path a
/// program that makes text spells is refused; one built from what any
/// other program prints is not seen, as one a variable names that this
/// reader does not know is not found.
fn script_of(program: &str, path: &str, runner: Runner, depth: usize) -> Reading {
    if path.contains(SPELLED) {
        return built(program);
    }
    if path.contains(BUILT) {
        return Reading::default();
    }
    Reading {
        scripts: vec![Script {
            path: path.to_string(),
            named: path.to_string(),
            sourced: matches!(program, "." | "source"),
            dir: String::new(),
            runner,
            depth,
            text: None,
        }],
        ..Reading::default()
    }
}

/// What one simple command of `words` changes and runs, `depth` strings
/// deep, given what it reads on its input and the variables the line set;
/// the command it resolves to comes first among the commands it runs.
pub(super) fn changed(
    words: &[String],
    input: Input,
    depth: usize,
    vars: &Variables,
    origin: &Origin,
) -> Reading {
    resolved(words, input, depth, vars, origin, false)
}

/// [`changed`], for a command a wrapper runs when `wrapped`.
fn resolved(
    words: &[String],
    input: Input,
    depth: usize,
    vars: &Variables,
    origin: &Origin,
    wrapped: bool,
) -> Reading {
    if depth > DEPTH {
        let mut reading = Reading {
            writes: words.to_vec(),
            ..Reading::default()
        };
        overflowed(&mut reading, &[words.to_vec()]);
        return reading;
    }
    let found = command(words);
    if let Some(replace) = found.xargs {
        return xargs(
            found.words,
            replace.as_deref(),
            input.text(),
            depth,
            vars,
            origin,
        );
    }
    // `sudo -s` with no command runs a shell, which reads its input.
    if found.words.is_empty() && found.shell {
        return run(&["sh".to_string()], input, depth, vars, origin);
    }
    let mut reading = run(found.words, input, depth, vars, origin);
    let Some(name) = found.words.first() else {
        return reading;
    };
    let unknown = unknown(name);
    if let (true, Input::Text(text)) = (unknown, input) {
        reading
            .commands
            .extend(told(code_of(name, text, depth, vars, origin)));
    }
    let resolved = Command {
        unresolved: unknown && (wrapped || found.wrapped),
        ..ran(found.words, &reading.writes)
    };
    reading.commands.insert(0, resolved);
    reading
}

/// Whether this reader does not know what the program `name` runs: it is
/// no shell, interpreter or other program whose commands it reads, and
/// none that runs only text.
fn unknown(name: &str) -> bool {
    let name = program(name);
    let lowercase = name.to_ascii_lowercase();
    !(TEXT_ONLY.contains(&name)
        || SHELLS.contains(&lowercase.as_str())
        || language(name).is_some()
        || matches!(
            name,
            "eval" | "trap" | "su" | "find" | "." | "source" | "ssh"
        )
        || matches!(
            lowercase.as_str(),
            "pwsh" | "powershell" | "pwsh.exe" | "powershell.exe" | "cmd" | "cmd.exe"
        ))
}

/// What the command `words`, its program first, changes and runs.
fn run(words: &[String], input: Input, depth: usize, vars: &Variables, origin: &Origin) -> Reading {
    let Some((name, args)) = words.split_first() else {
        return Reading::default();
    };
    // A program whose name a program that makes text spells may be any:
    // every word it is given may be a path it writes. One some other
    // program prints is as unknown as a variable that is not set.
    if program(name).contains(SPELLED) {
        return Reading {
            writes: args.to_vec(),
            ..Reading::default()
        };
    }
    if program(name).contains(BUILT) {
        return Reading::default();
    }
    // A system that does not tell case apart runs `BASH` as `bash`.
    let lowercase = program(name).to_ascii_lowercase();
    match program(name) {
        "eval" => code_of("eval", &args.join(" "), depth, vars, origin),
        // What `trap` is given runs when the shell gets a signal or exits.
        "trap" => match args.iter().find(|arg| *arg != "--") {
            Some(code) if !code.starts_with('-') => code_of("trap", code, depth, vars, origin),
            _ => Reading::default(),
        },
        "su" => {
            let parsed = parse(
                args,
                "cgGsw",
                &[
                    "command",
                    "session-command",
                    "group",
                    "supp-group",
                    "shell",
                    "whitelist-environment",
                ],
            );
            let mut codes: Vec<&str> = parsed.values(&["c", "command", "session-command"]);
            // With none, the shell it starts reads its commands from its
            // input.
            if let (true, Input::Text(text)) = (codes.is_empty(), input) {
                codes.push(text);
            }
            let mut reading = Reading::default();
            for code in codes {
                merge(
                    &mut reading,
                    code_of("su", code, depth, &Variables::default(), &Origin::default()),
                );
            }
            reading
        }
        "ssh" => ssh(args, input, depth),
        "find" => find(args, depth, vars, origin),
        "." | "source" => match args.first() {
            Some(path) => script_of(program(name), path, Runner::Shell, depth),
            None => Reading::default(),
        },
        _ if matches!(
            lowercase.as_str(),
            "pwsh" | "powershell" | "pwsh.exe" | "powershell.exe"
        ) =>
        {
            super::powershell::invoked(args, depth)
        }
        _ if matches!(lowercase.as_str(), "cmd" | "cmd.exe") => {
            super::powershell::cmd_invoked(args, depth)
        }
        // `$0` in the code a shell is given is no script's.
        shell if SHELLS.contains(&lowercase.as_str()) => match shell_code(args, input) {
            Given::Code(code) => {
                let origin = Origin {
                    dir: origin.dir.clone(),
                    ..Origin::default()
                };
                code_of(shell, &code, depth, vars, &origin)
            }
            Given::Script(path) => script_of(shell, &path, Runner::Shell, depth),
            Given::Nothing => Reading::default(),
        },
        program_name => {
            let mut reading = Reading {
                writes: written(program_name, args),
                ..Reading::default()
            };
            if let Some(language) = language(program_name) {
                let runner = Runner::Interpreter(program_name.to_string());
                merge(
                    &mut reading,
                    match interpreted(language, args, input) {
                        // Code a program that makes text spells may be
                        // any; code with what another program prints in
                        // it is judged by what it spells around that.
                        Given::Code(text) if text.contains(SPELLED) => built(program_name),
                        Given::Code(text) => Reading {
                            code: vec![Code {
                                program: program_name.to_string(),
                                text: text.replace(BUILT, " "),
                                dir: String::new(),
                            }],
                            ..Reading::default()
                        },
                        Given::Script(path) => script_of(program_name, &path, runner, depth),
                        Given::Nothing => Reading::default(),
                    },
                );
            } else if reading.writes.is_empty() && program_name.len() < name.len() {
                // A program named by its path may be a script.
                merge(
                    &mut reading,
                    script_of(program_name, name, Runner::Program, depth),
                );
            }
            reading
        }
    }
}

/// What `ssh` runs on the host it reaches: the command its words after the
/// destination spell, or with none the commands it is given on its input,
/// which the shell there reads. They are told for what they run: what they
/// write, and the repository a git among them changes, are that host's.
fn ssh(args: &[String], input: Input, depth: usize) -> Reading {
    // Letters whose value is the rest of their word or the next one.
    const VALUED: &str = "BbcDEeFIiJLlmOoPpQRSWw";
    let mut destination = false;
    let mut runs = true;
    let mut reads = true;
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        if arg == "--" {
            at += 1;
            if destination {
                break;
            }
        } else if arg.len() > 1 && arg.starts_with('-') {
            at += 1;
            for (index, letter) in arg.char_indices().skip(1) {
                match letter {
                    'n' | 'f' => reads = false,
                    'N' | 's' => runs = false,
                    _ => {}
                }
                if VALUED.contains(letter) {
                    at += usize::from(index + letter.len_utf8() == arg.len());
                    break;
                }
            }
        } else if destination {
            break;
        } else {
            destination = true;
            at += 1;
        }
    }
    let command = args.get(at..).unwrap_or_default();
    let code = match input {
        _ if !runs => return Reading::default(),
        _ if !command.is_empty() => command.join(" "),
        Input::Text(text) if reads => text.to_string(),
        _ => return Reading::default(),
    };
    let found = code_of(
        "ssh",
        &code,
        depth,
        &Variables::default(),
        &Origin::default(),
    );
    Reading {
        commands: told(found)
            .map(|command| Command {
                remote: true,
                ..command
            })
            .collect(),
        ..Reading::default()
    }
}

/// What `xargs` runs `words` as: with the items of its input appended, or
/// each in place of `replace`. Items come only from input the line spells.
fn xargs(
    words: &[String],
    replace: Option<&str>,
    input: Option<&str>,
    depth: usize,
    vars: &Variables,
    origin: &Origin,
) -> Reading {
    let mut reading = Reading::default();
    if words.is_empty() {
        return reading;
    }
    // The items past those read, written as they are.
    let mut left: Vec<String> = Vec::new();
    let items: Vec<&str> = input
        .map(|text| text.split_whitespace().collect())
        .unwrap_or_default();
    let runs: Vec<Vec<String>> = match replace.filter(|replace| !replace.is_empty()) {
        Some(replace) if !items.is_empty() => {
            left.extend(items.iter().skip(RUNS).map(|item| item.to_string()));
            reading.writes.extend(left.iter().cloned());
            items
                .iter()
                .take(RUNS)
                .map(|item| {
                    words
                        .iter()
                        .map(|word| word.replace(replace, item))
                        .collect()
                })
                .collect()
        }
        _ => vec![words
            .iter()
            .cloned()
            .chain(items.iter().map(|item| item.to_string()))
            .collect()],
    };
    for run in runs {
        merge(
            &mut reading,
            resolved(&run, Input::Unknown, depth + 1, vars, origin, true),
        );
    }
    // The first command run writes them, for a caller that asks which.
    if let Some(first) = reading.commands.first_mut() {
        first.writes.extend(left);
    }
    reading
}

/// What a command reads on its input, as far as the line spells it.
#[derive(Clone, Copy)]
pub(super) enum Input<'a> {
    /// Nothing the line spells.
    Unknown,
    /// Text: a here-document, a here-string, or what `echo` pipes.
    Text(&'a str),
    /// A file redirected into it with `<`.
    File(&'a str),
}

impl<'a> Input<'a> {
    pub(super) fn text(self) -> Option<&'a str> {
        match self {
            Input::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// What a shell or an interpreter is given to run.
enum Given {
    Code(String),
    /// A script file, by its path as written.
    Script(String),
    Nothing,
}

/// What a command that runs code reads it from when its arguments name
/// neither code nor a script: its input.
fn from_input(input: Input) -> Given {
    match input {
        Input::Text(text) => Given::Code(text.to_string()),
        Input::File(path) => Given::Script(path.to_string()),
        Input::Unknown => Given::Nothing,
    }
}

/// What a shell is given to run: the string after `-c`, its script, or its
/// input when it reads its commands from it.
fn shell_code(args: &[String], input: Input) -> Given {
    let mut code = false;
    let mut stdin = false;
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        if arg == "-" {
            stdin = true;
            at += 1;
            break;
        }
        if arg == "--" {
            at += 1;
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            if let Some(code) = long.strip_prefix("command=") {
                return Given::Code(code.to_string());
            }
            match long {
                "command" => {
                    return args
                        .get(at + 1)
                        .map_or(Given::Nothing, |code| Given::Code(code.clone()))
                }
                "rcfile" | "init-file" => at += 2,
                _ => at += 1,
            }
            continue;
        }
        if arg.len() > 1 && (arg.starts_with('-') || arg.starts_with('+')) {
            let letters = &arg[1..];
            code |= letters.contains('c');
            stdin |= letters.contains('s');
            // `-o pipefail` and `-O extglob` take a value.
            at += 1 + letters.chars().filter(|c| matches!(c, 'o' | 'O')).count();
            continue;
        }
        break;
    }
    match args.get(at) {
        Some(operand) if code => Given::Code(operand.clone()),
        Some(script) if !stdin => Given::Script(script.clone()),
        _ if code => Given::Nothing,
        _ => from_input(input),
    }
}

/// What an interpreter in `language` is given to run: the code its options
/// carry, else its script, else what is on its input. A module it runs by
/// name, as `python -m`, is neither.
fn interpreted(language: &Language, args: &[String], input: Input) -> Given {
    let takes = |name: &str| {
        language.code.contains(&name)
            || language.script.contains(&name)
            || language.valued.contains(&name)
    };
    let mut code: Vec<String> = Vec::new();
    let mut options = true;
    let mut subcommand: Option<&str> = None;
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        at += 1;
        if options && arg == "--" {
            options = false;
            continue;
        }
        if options && arg == "-" {
            break;
        }
        if !(options && arg.len() > 1 && arg.starts_with('-')) {
            if language.subcommands
                && subcommand.is_none()
                && code.is_empty()
                && matches!(arg.as_str(), "eval" | "run")
            {
                subcommand = Some(arg);
                continue;
            }
            if subcommand == Some("eval") {
                code.push(arg.clone());
            } else if code.is_empty() {
                return Given::Script(arg.clone());
            }
            break;
        }
        // The options this word gives, each with the value written in it.
        let mut given: Vec<(&str, Option<&str>)> = Vec::new();
        if let Some(long) = arg.strip_prefix("--") {
            given.push(match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            });
        } else {
            let letters = &arg[1..];
            for (index, letter) in letters.char_indices() {
                if language.attached.contains(letter) {
                    break;
                }
                let end = index + letter.len_utf8();
                let (name, rest) = (&letters[index..end], &letters[end..]);
                if !takes(name) {
                    given.push((name, None));
                } else if language.glued {
                    given.push((name, Some(rest).filter(|rest| !rest.is_empty())));
                    break;
                } else {
                    given.push((name, None));
                }
            }
        }
        // Those that take a value and have none in the word share the next.
        let mut next: Option<Option<&String>> = None;
        for (name, value) in given {
            if takes(name) {
                let value = match value {
                    Some(value) => Some(value),
                    None => next
                        .get_or_insert_with(|| {
                            at += 1;
                            args.get(at - 1)
                        })
                        .map(String::as_str),
                };
                if let Some(value) = value {
                    if language.code.contains(&name) {
                        code.push(value.to_string());
                    } else if language.script.contains(&name) {
                        return Given::Script(value.to_string());
                    }
                }
            }
            if language.ends.contains(&name) {
                return if code.is_empty() {
                    Given::Nothing
                } else {
                    Given::Code(code.join("\n"))
                };
            }
        }
    }
    if code.is_empty() {
        from_input(input)
    } else {
        Given::Code(code.join("\n"))
    }
}

/// What `find` changes: the directories it searches when it deletes or
/// runs a command that changes what it finds, and the files it prints to.
fn find(args: &[String], depth: usize, vars: &Variables, origin: &Origin) -> Reading {
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        match arg.as_str() {
            "-H" | "-L" | "-P" => at += 1,
            "-D" => at += 2,
            option if option.starts_with("-O") => at += 1,
            _ => break,
        }
    }
    let start = at.min(args.len());
    while args
        .get(at)
        .is_some_and(|arg| !arg.starts_with('-') && !matches!(arg.as_str(), "(" | "!" | ","))
    {
        at += 1;
    }
    let roots = if at > start {
        args[start..at].to_vec()
    } else {
        vec![".".to_string()]
    };
    let mut reading = Reading::default();
    let mut changes = false;
    while at < args.len() {
        match args[at].as_str() {
            "-delete" => changes = true,
            "-fprint" | "-fprint0" | "-fprintf" | "-fls" => {
                reading.writes.extend(args.get(at + 1).cloned());
                at += 1;
            }
            "-exec" | "-execdir" | "-ok" | "-okdir" => {
                let end = (at + 1..args.len())
                    .find(|end| args[*end] == ";" || (args[*end] == "+" && args[*end - 1] == "{}"))
                    .unwrap_or(args.len());
                let mut found =
                    changed(&args[at + 1..end], Input::Unknown, depth + 1, vars, origin);
                changes |= !found.writes.is_empty();
                let writes = std::mem::take(&mut found.writes);
                reading
                    .writes
                    .extend(writes.into_iter().filter(|path| !path.contains("{}")));
                // What it is given to run is each file found.
                found.scripts.retain(|script| !script.path.contains("{}"));
                merge(&mut reading, found);
                at = end;
            }
            _ => {}
        }
        at += 1;
    }
    if changes {
        reading.writes.extend(roots);
    }
    reading
}
