//! What one statement changes and runs: cmdlets, native programs and
//! nested PowerShell or `cmd`.

use crate::core::reading::{Reading, Runner, Script, Unread, DEPTH};
use crate::core::shell;

use super::cmdlets::{
    command_name, Argument, Bound, Kind, Piped, CMDLETS, EVALUATORS, PATH_PARAMETERS,
};
use super::paths::{base, file_path, joined};
use super::read::read_at;

/// Cmdlets whose file is never piped in: what is piped is what they write.
const WRITES_INPUT: &[&str] = &[
    "out-file",
    "tee-object",
    "tee",
    "export-csv",
    "epcsv",
    "export-clixml",
];
/// Cmdlets a piped string is the content of, not the path to.
const CONTENT_INPUT: &[&str] = &["set-content", "sc", "add-content", "ac", "new-item", "ni"];

/// What a cmdlet or a native program changes through its arguments; a
/// cmdlet given no path changes what is piped into it, `input`.
pub(super) fn changed_at(arguments: &[Argument], input: &Piped, depth: usize) -> Reading {
    // `$x = Remove-Item y`: the command is past the assignment.
    let arguments = match arguments {
        [variable, operator, rest @ ..]
            if variable.len() == 1
                && variable[0].0.starts_with('$')
                && matches!(operator.as_slice(), [(op, false)] if op.ends_with('=')) =>
        {
            rest
        }
        _ => arguments,
    };
    let Some(first) = arguments.first().and_then(|argument| argument.first()) else {
        return Reading::default();
    };
    let name = command_name(&first.0);
    if EVALUATORS.contains(&name.as_str()) {
        // `iex "Remove-Item x"` runs its string, or the one piped into it.
        let code: Vec<&str> = arguments[1..]
            .iter()
            .flatten()
            .filter(|(word, quoted)| *quoted || !word.starts_with('-'))
            .map(|(word, _)| word.as_str())
            .collect();
        let code = if code.is_empty() {
            input.paths.join("\n")
        } else {
            code.join(" ")
        };
        return read_at(&code, depth + 1);
    }
    if matches!(name.as_str(), "start-process" | "start" | "saps") {
        return started(&arguments[1..], depth);
    }
    // A script runs in this shell, `&` or `.` before it or not.
    if name.ends_with(".ps1") {
        return Reading {
            scripts: file_path(&first.0)
                .map(|path| Script {
                    named: path.clone(),
                    sourced: false,
                    path,
                    dir: String::new(),
                    runner: Runner::PowerShell,
                    depth,
                    text: None,
                })
                .into_iter()
                .collect(),
            ..Reading::default()
        };
    }
    let Some(kind) = CMDLETS
        .iter()
        .find(|(names, _)| names.contains(&name.as_str()))
        .map(|(_, kind)| *kind)
    else {
        let words: Vec<String> = std::iter::once(name)
            .chain(
                arguments
                    .iter()
                    .flatten()
                    .skip(1)
                    .map(|(word, _)| word.clone()),
            )
            .collect();
        return native(&words, depth);
    };
    let bound = Bound::new(&arguments[1..]);
    let mut paths = bound.values(PATH_PARAMETERS, 0);
    let name = name.as_str();
    let binds = !WRITES_INPUT.contains(&name) && (input.items || !CONTENT_INPUT.contains(&name));
    if paths.is_empty() && kind != Kind::Downloads && binds {
        paths = input.paths.clone();
    }
    // A positional argument binds to the first position not bound by name:
    // with `-Path` named, the first one is the destination or new name.
    let second = if bound
        .named
        .iter()
        .any(|(name, _)| PATH_PARAMETERS.contains(name))
    {
        0
    } else {
        1
    };
    let mut changed = match kind {
        Kind::Changes => paths,
        Kind::Creates => {
            let names = bound.values(&["name"], usize::MAX);
            if names.is_empty() {
                paths
            } else {
                let bases = if paths.is_empty() {
                    vec![String::new()]
                } else {
                    paths
                };
                bases
                    .iter()
                    .flat_map(|base| names.iter().map(move |name| joined(base, name)))
                    .collect()
            }
        }
        Kind::Moves | Kind::Copies => {
            let mut changed = if kind == Kind::Moves {
                paths.clone()
            } else {
                Vec::new()
            };
            for destination in bound.values(&["destination"], second) {
                changed.extend(into(&destination, &paths));
            }
            changed
        }
        Kind::Renames => {
            let mut changed = paths.clone();
            for new_name in bound.values(&["newname"], second) {
                for source in &paths {
                    let dir = source.rsplit_once(['/', '\\']).map_or("", |(dir, _)| dir);
                    changed.push(joined(dir, &new_name));
                }
            }
            changed
        }
        Kind::Downloads => bound.values(&["outfile"], usize::MAX),
    };
    if bound.unknown {
        changed.extend(bound.positional.iter().flatten().cloned());
    }
    Reading {
        writes: changed,
        ..Reading::default()
    }
}

/// What a native program changes: the string `cmd /c` or `powershell
/// -Command` runs, read again; any other as the shell reader reads it.
fn native(words: &[String], depth: usize) -> Reading {
    let Some((name, args)) = words.split_first() else {
        return Reading::default();
    };
    let name = command_name(name);
    match name.as_str() {
        "cmd" => {
            let code = args.iter().position(|arg| {
                ["/c", "/k", "/r"]
                    .iter()
                    .any(|flag| arg.eq_ignore_ascii_case(flag))
            });
            Reading {
                writes: code
                    .map(|at| cmd(&args[at + 1..].join(" "), depth + 1))
                    .unwrap_or_default(),
                ..Reading::default()
            }
        }
        "powershell" | "pwsh" => invoked(args, depth),
        _ => {
            let words: Vec<String> = std::iter::once(name).chain(args.iter().cloned()).collect();
            shell::changed_by(&words, depth)
        }
    }
}

/// What `powershell` or `pwsh` is given to run.
enum Given {
    /// Code: after `-Command`, or the arguments after its options.
    Command(String),
    /// Code as `-EncodedCommand` carries it, base64 of UTF-16 text.
    Encoded(String),
    /// A script: after `-File`, or a first argument that names one.
    File(String),
    Nothing,
}

/// What `powershell` or `pwsh` given `args` runs.
fn given(args: &[String]) -> Given {
    const VALUES: &[&str] = &[
        "executionpolicy",
        "ep",
        "ex",
        "windowstyle",
        "w",
        "workingdirectory",
        "wd",
        "inputformat",
        "if",
        "inp",
        "outputformat",
        "of",
        "o",
        "configurationname",
        "config",
        "version",
        "v",
        "settingsfile",
        "settings",
        "custompipename",
        "encodedarguments",
        "ea",
    ];
    let prefix = |flag: &str, name: &str| flag.len() > 1 && name.starts_with(flag);
    let mut at = 0;
    while let Some(flag) = args.get(at).and_then(|arg| arg.strip_prefix('-')) {
        let flag = flag.to_ascii_lowercase();
        at += 1;
        if flag == "c" || (flag.len() > 2 && "command".starts_with(&flag)) {
            break;
        }
        // `-enc`, though `-EncodedArguments` starts so too, is taken as
        // PowerShell takes it: for the code.
        if flag == "e" || flag == "ec" || prefix(&flag, "encodedcommand") {
            return args
                .get(at)
                .map_or(Given::Nothing, |code| Given::Encoded(code.clone()));
        }
        if VALUES.contains(&flag.as_str())
            || (flag.len() > 2 && VALUES.iter().any(|name| name.starts_with(&flag)))
        {
            at += 1;
        } else if flag == "f" || prefix(&flag, "file") {
            return args
                .get(at)
                .map_or(Given::Nothing, |path| Given::File(path.clone()));
        }
    }
    match args.get(at) {
        Some(first) if first.to_ascii_lowercase().ends_with(".ps1") => Given::File(first.clone()),
        Some(_) => Given::Command(args[at..].join(" ")),
        None => Given::Nothing,
    }
}

/// The text base64 `encoded` carries as UTF-16, little-endian, as
/// `-EncodedCommand` takes it; `None` when it carries none.
fn decoded(encoded: &str) -> Option<String> {
    let bytes = super::digest::base64_decode(encoded).ok()?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

/// What `powershell` or `pwsh` given `args` runs, `depth` strings deep:
/// its code read again, encoded code decoded first, or the script it is
/// given, for the caller to read.
pub(crate) fn invoked(args: &[String], depth: usize) -> Reading {
    match given(args) {
        Given::Command(code) => read_at(&code, depth + 1),
        Given::Encoded(encoded) => match decoded(&encoded) {
            Some(code) => read_at(&code, depth + 1),
            None => Reading {
                unread: vec![Unread {
                    shape: "powershell -EncodedCommand".to_string(),
                    why: "what it is given is not base64 of UTF-16 text, so flot cannot read the code it runs".to_string(),
                }],
                ..Reading::default()
            },
        },
        Given::File(path) => Reading {
            scripts: file_path(&path)
                .map(|path| Script {
                    named: path.clone(),
                    sourced: false,
                    path,
                    dir: String::new(),
                    runner: Runner::PowerShell,
                    depth,
                    text: None,
                })
                .into_iter()
                .collect(),
            ..Reading::default()
        },
        Given::Nothing => Reading::default(),
    }
}

/// What `Start-Process` given `arguments` changes and runs: the program
/// after `-FilePath` with the arguments after `-ArgumentList`, read as if
/// run here, and the files it sends its output to. One string of
/// arguments is a command line, split as the program would split it.
fn started(arguments: &[Argument], depth: usize) -> Reading {
    /// Its parameters by name and alias: whether each is the program, its
    /// arguments, a file it writes, or another that takes a value; any
    /// other is a switch.
    const PARAMETERS: &[(&str, &str)] = &[
        ("filepath", "program"),
        ("path", "program"),
        ("pspath", "program"),
        ("argumentlist", "arguments"),
        ("args", "arguments"),
        ("redirectstandardoutput", "writes"),
        ("rso", "writes"),
        ("redirectstandarderror", "writes"),
        ("rse", "writes"),
        ("redirectstandardinput", "value"),
        ("rsi", "value"),
        ("workingdirectory", "value"),
        ("verb", "value"),
        ("windowstyle", "value"),
        ("credential", "value"),
        ("environment", "value"),
    ];
    let mut program: Option<String> = None;
    let mut list: Vec<String> = Vec::new();
    let mut writes: Vec<String> = Vec::new();
    let mut positional = 0;
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        let words: Vec<String> = argument.iter().map(|(word, _)| word.clone()).collect();
        let flag = match argument.as_slice() {
            [(word, false)] if word.len() > 1 && word.starts_with('-') => Some(&word[1..]),
            _ => None,
        };
        let role = match flag {
            Some(flag) => {
                let (written, inline) = match flag.split_once(':') {
                    Some((written, value)) => (written.to_ascii_lowercase(), Some(value)),
                    None => (flag.to_ascii_lowercase(), None),
                };
                let exact = PARAMETERS.iter().find(|(name, _)| *name == written);
                let mut prefixed = PARAMETERS
                    .iter()
                    .filter(|(name, _)| name.starts_with(&written));
                let role = exact
                    .or_else(|| match (prefixed.next(), prefixed.next()) {
                        (Some(only), None) => Some(only),
                        _ => None,
                    })
                    .map(|(_, role)| *role);
                let Some(role) = role else {
                    continue;
                };
                let value = match inline {
                    Some(value) => vec![value.to_string()],
                    None => rest
                        .next()
                        .map(|value| value.iter().map(|(word, _)| word.clone()).collect())
                        .unwrap_or_default(),
                };
                (role, value)
            }
            None => {
                positional += 1;
                match positional {
                    1 => ("program", words),
                    2 => ("arguments", words),
                    _ => continue,
                }
            }
        };
        match role {
            ("program", value) => program = value.into_iter().next(),
            ("arguments", value) => list = value,
            ("writes", value) => writes.extend(value),
            _ => {}
        }
    }
    let mut reading = Reading {
        writes,
        ..Reading::default()
    };
    let Some(program) = program else {
        return reading;
    };
    let args = match list.as_slice() {
        [line] => command_line(line),
        _ => list,
    };
    let run: Vec<Argument> = std::iter::once(program)
        .chain(args)
        .map(|word| vec![(word, false)])
        .collect();
    let found = changed_at(&run, &Piped::default(), depth + 1);
    reading.writes.extend(found.writes);
    reading.remote_code = found.remote_code;
    reading.code = found.code;
    reading.scripts = found.scripts;
    reading.unread = found.unread;
    reading
}

/// `line` split into arguments as a Windows program splits its command
/// line: at whitespace, with double quotes keeping it together.
fn command_line(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// What `cmd /c` runs, read with cmd's rules: `&`, `&&`, `||`, `|` and
/// parentheses separate commands, `"` quotes, `^` escapes, and `>` writes
/// the word after it.
fn cmd(command: &str, depth: usize) -> Vec<String> {
    if depth > DEPTH {
        return command.split_whitespace().map(str::to_string).collect();
    }
    let chars: Vec<char> = command.chars().collect();
    let mut writes = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    // The word being read is a redirection's target, `Some(true)`, or a
    // file only read, `Some(false)`.
    let mut redirect: Option<bool> = None;
    let mut at = 0;
    loop {
        let c = chars.get(at).copied();
        // `2>` names a stream, not a word.
        if c == Some('>') && !quoted && matches!(word.as_str(), "1" | "2") {
            word.clear();
        }
        let ends = matches!(
            c,
            None | Some(' ' | '\t' | '&' | '|' | '(' | ')' | '\n' | '\r' | '<' | '>')
        );
        if ends && (!word.is_empty() || quoted) {
            let done = std::mem::take(&mut word);
            match redirect.take() {
                Some(true) if !done.eq_ignore_ascii_case("nul") => writes.push(done),
                Some(_) => {}
                None => words.push(done),
            }
            quoted = false;
        }
        match c {
            None => {
                writes.extend(builtin(&words, depth));
                break;
            }
            Some('"') => {
                quoted = true;
                at += 1;
                while at < chars.len() && chars[at] != '"' {
                    word.push(chars[at]);
                    at += 1;
                }
            }
            Some('^') => {
                at += 1;
                word.extend(chars.get(at));
            }
            Some('&' | '|' | '(' | ')' | '\n' | '\r') => {
                writes.extend(builtin(&words, depth));
                words.clear();
                if matches!(c, Some('&' | '|')) && chars.get(at + 1).copied() == c {
                    at += 1;
                }
            }
            Some('>') => {
                if chars.get(at + 1) == Some(&'>') {
                    at += 1;
                }
                if chars.get(at + 1) == Some(&'&') {
                    // `2>&1` merges a stream.
                    at += 2;
                } else {
                    redirect = Some(true);
                }
            }
            Some('<') => redirect = Some(false),
            Some(' ' | '\t') => {}
            Some(other) => word.push(other),
        }
        at += 1;
    }
    writes
}

/// What one command `cmd` runs changes: its built-ins `del`, `rd`, `copy`,
/// `move` and `ren` by cmd's rules, any other program as [`native`] reads
/// it.
fn builtin(words: &[String], depth: usize) -> Vec<String> {
    let Some((name, args)) = words.split_first() else {
        return Vec::new();
    };
    let operands: Vec<String> = args
        .iter()
        .filter(|arg| !arg.starts_with('/'))
        .cloned()
        .collect();
    let name = command_name(name.trim_start_matches('@'));
    match name.as_str() {
        "del" | "erase" | "rd" | "rmdir" => operands,
        "copy" | "move" => {
            let Some((destination, sources)) = operands.split_last() else {
                return Vec::new();
            };
            if sources.is_empty() {
                // `copy x` copies it here, or what it holds when it is a
                // directory.
                return vec![base(destination).to_string(), ".".to_string()];
            }
            let mut written = if name == "move" {
                sources.to_vec()
            } else {
                // A directory copied puts what it holds in the destination.
                vec![destination.clone()]
            };
            written.extend(into(destination, sources));
            written
        }
        // What the source holds lands in the destination, by default here.
        "xcopy" | "robocopy" => match operands.as_slice() {
            [_] => vec![".".to_string()],
            [_, destination, ..] => vec![destination.clone()],
            [] => Vec::new(),
        },
        "ren" | "rename" => match operands.as_slice() {
            [path, new_name, ..] => {
                let dir = path.rsplit_once(['/', '\\']).map_or("", |(dir, _)| dir);
                vec![path.clone(), joined(dir, new_name)]
            }
            _ => Vec::new(),
        },
        "mklink" => operands.first().cloned().into_iter().collect(),
        "echo" | "rem" | "type" | "dir" | "cd" | "chdir" | "pushd" | "popd" | "set" => Vec::new(),
        _ => native(words, depth).writes,
    }
}

/// What copying or moving `sources` to `destination` writes: the
/// destination when it may be the file written, and each source's name in
/// it when it may be a directory; a trailing separator, `.` or several
/// sources make it one.
fn into(destination: &str, sources: &[String]) -> Vec<String> {
    let directory = destination.ends_with(['/', '\\'])
        || matches!(destination, "." | "..")
        || sources.len() > 1;
    let mut written = if directory {
        Vec::new()
    } else {
        vec![destination.to_string()]
    };
    written.extend(
        sources
            .iter()
            .map(|source| joined(destination, base(source))),
    );
    written
}
