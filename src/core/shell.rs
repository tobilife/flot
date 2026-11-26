//! What a shell command line visibly does to files, read without running it.
//!
//! This is not a shell. It splits a command the way `sh` would split words,
//! quotes, comments and operators, and recognises literal shapes: a
//! redirection into a file; a program that changes the files it is given
//! (`rm`, `sed -i`, `tee`, `git checkout --`, `curl -o`, `tar -x`, ...),
//! also behind reserved words such as `if` or `!` and wrappers such as
//! `sudo`, `timeout` or `xargs`; code handed to a shell as a string
//! (`eval`, `sh -c`, `$(..)`, a here-document or an `echo` piped into
//! `sh`), read again; and code fetched from the network piped into an
//! interpreter. A `cd` moves the relative paths after it, and a variable
//! the line sets (`f=x; rm $f`, `for f in x`) is put in where it is used
//! after. Code given to an interpreter (`python3 -c`, `node -e`, a
//! here-document piped into `perl`) and the script files a shell or an
//! interpreter runs are handed back for the caller to look into: this
//! module never reads the disk. A command whose code or program is built
//! when the line runs (`sh -c "$(..)"`) is named as one no reading can
//! know. A command that writes some other way, through a program that does
//! it on its own or a variable set before the line, is not seen. Seeing
//! less is the point: what is reported is a write the command line itself
//! names.

use std::collections::HashSet;

/// What a command line names.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// Paths it writes, deletes, moves or changes the mode of, as written:
    /// a glob, a brace, `$PWD`, `$HOME`, `~` or a directory is the caller's
    /// to expand, `.` stands for the whole tree (`git stash`, `tar -x`), and
    /// [`BUILT`] for a part built when the line runs. A sed script or an
    /// option's value can be among them: a caller looks for the files it
    /// cares about, not for what is missing.
    pub writes: Vec<String>,
    /// `fetcher | interpreter` when it pipes downloaded code into one.
    pub remote_code: Option<String>,
    /// Code given to an interpreter on its command line or its input.
    pub code: Vec<Code>,
    /// Script files a shell or an interpreter is given to run.
    pub scripts: Vec<Script>,
    /// Commands whose code no reading of the line can see.
    pub unread: Vec<Unread>,
    /// Every word the line's commands are given, and the files redirected
    /// into them, located as `writes` are: where a caller looks for a file
    /// that must not even be read.
    pub named: Vec<String>,
}

/// Code an interpreter is given, which this reader does not read: only the
/// files it names can be looked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    /// The interpreter, as `python3`.
    pub program: String,
    pub text: String,
    /// The directory it runs in, from where the line starts; empty for there.
    pub dir: String,
}

/// A script file a command runs, for the caller to read from the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// Its path as written, from where the line starts.
    pub path: String,
    /// The directory it runs in, from where the line starts; empty for there.
    pub dir: String,
    pub runner: Runner,
    /// How many times the code naming it was handed over as a string: its
    /// own text is read one deeper.
    pub depth: usize,
    /// What the line writes into it before running it, when the line
    /// spells that: a here-document or an `echo` redirected into it.
    pub text: Option<String>,
}

/// What runs a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runner {
    /// A POSIX shell, which reads it as commands.
    Shell,
    /// PowerShell.
    PowerShell,
    /// An interpreter, by its name: its text is code.
    Interpreter(String),
    /// The system, as a program: its `#!` line names what runs it, and a
    /// text file with none is run by the shell.
    Program,
}

/// A command whose code this reader cannot see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unread {
    /// The command as spelled, such as `sh $(..)`.
    pub shape: String,
    /// Why its code cannot be seen, said of the command.
    pub why: String,
}

/// What a word holds in place of a command substituted in it, `$(..)` or
/// backquotes, whose output no reading of the line can know.
pub const BUILT: char = '\u{FFFC}';

/// What `$(mktemp ..)` stands for in a word: `mktemp` prints the name of a
/// file or directory it has just created, so the name is no file that was
/// there before, and the agent's `T=$(mktemp -d); ...; rm -rf $T` names
/// nothing a flow protects. The name is a fixed one outside any project.
pub const MKTEMP: &str = "/tmp/flot-mktemp.XXXXXX";

/// Put what the substituted command `inner` prints into `word`: the name
/// `mktemp` makes, or [`BUILT`] for output no reading of the line can know.
fn push_printed(word: &mut String, inner: &[char]) {
    let inner: String = inner.iter().collect();
    let program = inner.split_whitespace().next().unwrap_or_default();
    let simple = !inner.contains([';', '&', '|', '<', '>', '$', '`', '(', '\n']);
    if simple && matches!(program, "mktemp" | "/usr/bin/mktemp" | "/bin/mktemp") {
        word.push_str(MKTEMP);
    } else {
        word.push(BUILT);
    }
}

/// Why a command whose code is built when the line runs is not seen.
const BUILT_CODE: &str =
    "what it runs is built when the command runs, so flot cannot read it before it runs";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    /// `|`: the next command reads this one's output.
    Pipe,
    /// `&&`: the next command runs only if this one succeeded.
    And,
    /// `;`, `||`, `&` or a newline.
    Break,
    /// `(`: a subshell, whose `cd` and variables stay inside it.
    Open,
    /// `)`.
    Close,
    /// `>`, `>>`, `>|`, `2>`, `&>`, `>&`, `<>`: the next word is written.
    WriteTo,
    /// `<`: the next word is the command's input, only read.
    ReadFrom,
    /// `<<<`: the next word is the command's input.
    HereString,
    /// A here-document's body: the command's input.
    Input(String),
    /// A command substituted in a word, an arithmetic expression or a
    /// here-document body, to be read on its own.
    Nested(String),
    /// `<(`: a nested command whose output the command around it reads,
    /// as a file the shell names.
    Feeds,
}

/// A here-document whose body starts at the next newline.
struct Heredoc {
    delimiter: String,
    strip_tabs: bool,
    /// A quoted delimiter keeps the body literal: no `$(..)` runs in it.
    literal: bool,
    /// Where in the tokens its command reads it.
    position: usize,
}

/// How far `((` looks for the `))` that makes it arithmetic.
const ARITHMETIC: usize = 1024;

/// Where the parenthesis closes that leaves none of `depth` open, from
/// `at` on, past quotes and escapes and at most `limit` characters ahead;
/// the end of `chars` when none does.
fn closing(chars: &[char], mut at: usize, mut depth: usize, limit: usize) -> usize {
    let stop = chars.len().min(at.saturating_add(limit));
    while at < stop {
        match chars[at] {
            '\\' => at += 1,
            quote @ ('\'' | '"') => {
                at += 1;
                while at < stop && chars[at] != quote {
                    if quote == '"' && chars[at] == '\\' {
                        at += 1;
                    }
                    at += 1;
                }
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return at;
                }
            }
            _ => {}
        }
        at += 1;
    }
    chars.len()
}

/// Where the backquote is that closes one opened before `at`.
fn backquoted(chars: &[char], mut at: usize) -> usize {
    while at < chars.len() && chars[at] != '`' {
        if chars[at] == '\\' {
            at += 1;
        }
        at += 1;
    }
    at.min(chars.len())
}

/// Whether `$(inner)` or `(inner)` is arithmetic, `$((..))` or `((..))`.
fn arithmetic(inner: &[char]) -> bool {
    inner.first() == Some(&'(') && inner.last() == Some(&')')
}

/// The commands substituted in `text` by `$(..)` or backquotes, where
/// quotes are only text: a here-document body or arithmetic. Past `DEPTH`
/// nested arithmetic, `text` itself is taken for one.
fn substitutions(text: &[char], level: usize) -> Vec<String> {
    if level > DEPTH {
        return vec![text.iter().collect()];
    }
    let mut found = Vec::new();
    let mut at = 0;
    while at < text.len() {
        match text[at] {
            '\\' => at += 1,
            '$' if text.get(at + 1) == Some(&'(') => {
                let close = closing(text, at + 2, 1, usize::MAX);
                found.extend(substituted(&text[at + 2..close], level + 1));
                at = close;
            }
            '`' => {
                let close = backquoted(text, at + 1);
                found.push(text[at + 1..close].iter().collect());
                at = close;
            }
            _ => {}
        }
        at += 1;
    }
    found
}

/// The commands `$(inner)` runs: itself, or those substituted in it when
/// it is arithmetic.
fn substituted(inner: &[char], level: usize) -> Vec<String> {
    if arithmetic(inner) {
        substitutions(inner, level)
    } else {
        vec![inner.iter().collect()]
    }
}

/// Adds to `word` what the C escape at the start of `chars`, past its
/// backslash, stands for in `$'..'`; how many characters it takes.
fn escape(chars: &[char], word: &mut String) -> usize {
    let Some(&c) = chars.first() else {
        return 0;
    };
    let digits = |radix: u32, most: usize, from: usize| {
        chars[from..]
            .iter()
            .take(most)
            .take_while(|c| c.is_digit(radix))
            .count()
    };
    let code = |from: usize, count: usize, radix: u32| {
        let digits: String = chars[from..from + count].iter().collect();
        u32::from_str_radix(&digits, radix)
            .ok()
            .and_then(char::from_u32)
    };
    match c {
        'n' => word.push('\n'),
        't' => word.push('\t'),
        'r' => word.push('\r'),
        'a' => word.push('\u{7}'),
        'b' => word.push('\u{8}'),
        'e' | 'E' => word.push('\u{1b}'),
        'f' => word.push('\u{c}'),
        'v' => word.push('\u{b}'),
        '\\' | '\'' | '"' | '?' => word.push(c),
        'x' | 'u' | 'U' => {
            let most = match c {
                'x' => 2,
                'u' => 4,
                _ => 8,
            };
            let count = digits(16, most, 1);
            if count == 0 {
                word.push('\\');
                word.push(c);
                return 1;
            }
            word.extend(code(1, count, 16));
            return 1 + count;
        }
        '0'..='7' => {
            let count = digits(8, 3, 0);
            word.extend(code(0, count, 8));
            return count;
        }
        'c' if chars.len() > 1 => {
            word.extend(char::from_u32(chars[1] as u32 & 0x1f));
            return 2;
        }
        _ => {
            word.push('\\');
            word.push(c);
        }
    }
    1
}

fn tokens(command: &str) -> Vec<Token> {
    let chars: Vec<char> = command.chars().collect();
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut heredocs: Vec<Heredoc> = Vec::new();
    let mut at = 0;
    let end_word = |word: &mut String, quoted: &mut bool, out: &mut Vec<Token>| {
        if !word.is_empty() || *quoted {
            out.push(Token::Word(std::mem::take(word)));
        }
        *quoted = false;
    };
    while at < chars.len() {
        let c = chars[at];
        let next = chars.get(at + 1).copied();
        match c {
            '\'' => {
                quoted = true;
                at += 1;
                while at < chars.len() && chars[at] != '\'' {
                    word.push(chars[at]);
                    at += 1;
                }
            }
            '"' => {
                quoted = true;
                at += 1;
                while at < chars.len() && chars[at] != '"' {
                    match chars[at] {
                        '\\' if at + 1 < chars.len() => {
                            at += 1;
                            word.push(chars[at]);
                        }
                        // A command substituted in double quotes still runs.
                        '$' if chars.get(at + 1) == Some(&'(') => {
                            let close = closing(&chars, at + 2, 1, usize::MAX);
                            let inner = &chars[at + 2..close];
                            out.extend(substituted(inner, 0).into_iter().map(Token::Nested));
                            if arithmetic(inner) {
                                word.extend(&chars[at..chars.len().min(close + 1)]);
                            } else {
                                push_printed(&mut word, inner);
                            }
                            at = close;
                        }
                        '`' => {
                            let close = backquoted(&chars, at + 1);
                            out.push(Token::Nested(chars[at + 1..close].iter().collect()));
                            push_printed(&mut word, &chars[at + 1..close]);
                            at = close;
                        }
                        other => word.push(other),
                    }
                    at += 1;
                }
            }
            '$' if next == Some('\'') => {
                // `$'..'` quotes with C escapes.
                quoted = true;
                at += 2;
                while at < chars.len() && chars[at] != '\'' {
                    if chars[at] == '\\' && at + 1 < chars.len() {
                        at += escape(&chars[at + 1..], &mut word);
                    } else {
                        word.push(chars[at]);
                    }
                    at += 1;
                }
            }
            // `$".."` is a double-quoted string.
            '$' if next == Some('"') => {}
            '$' if next == Some('(') => {
                let close = if chars.get(at + 2) == Some(&'(') {
                    closing(&chars, at + 2, 1, ARITHMETIC)
                } else {
                    chars.len()
                };
                if close < chars.len() && arithmetic(&chars[at + 2..close]) {
                    // `$((..))` is arithmetic: a word, and no `<<` in it
                    // starts a here-document.
                    out.extend(
                        substitutions(&chars[at + 2..close], 0)
                            .into_iter()
                            .map(Token::Nested),
                    );
                    word.extend(&chars[at..=close]);
                    at = close;
                } else {
                    // What the command substituted prints is part of the
                    // word, which goes on after it.
                    let close = closing(&chars, at + 2, 1, usize::MAX);
                    out.push(Token::Nested(chars[at + 2..close].iter().collect()));
                    push_printed(&mut word, &chars[at + 2..close.min(chars.len())]);
                    at = close;
                }
            }
            '`' => {
                let close = backquoted(&chars, at + 1);
                out.push(Token::Nested(chars[at + 1..close].iter().collect()));
                push_printed(&mut word, &chars[at + 1..close]);
                at = close;
            }
            '\\' => {
                at += 1;
                if at < chars.len() && chars[at] != '\n' {
                    word.push(chars[at]);
                }
            }
            // A comment runs to the end of the line.
            '#' if word.is_empty() && !quoted => {
                while at + 1 < chars.len() && chars[at + 1] != '\n' {
                    at += 1;
                }
            }
            ' ' | '\t' => end_word(&mut word, &mut quoted, &mut out),
            '\n' => {
                end_word(&mut word, &mut quoted, &mut out);
                out.push(Token::Break);
                // A here-document's body is its command's input, not
                // commands; unless its delimiter is quoted, what `$(..)`
                // substitutes in it runs.
                let mut bodies = Vec::new();
                let mut nested = Vec::new();
                for heredoc in std::mem::take(&mut heredocs) {
                    let mut lines: Vec<String> = Vec::new();
                    loop {
                        let start = at + 1;
                        if start >= chars.len() {
                            at = chars.len();
                            break;
                        }
                        let stop = chars[start..]
                            .iter()
                            .position(|c| *c == '\n')
                            .map_or(chars.len(), |offset| start + offset);
                        let line: String = chars[start..stop].iter().collect();
                        at = stop;
                        let line = if heredoc.strip_tabs {
                            line.trim_start_matches('\t')
                        } else {
                            &line
                        };
                        if line.trim_end_matches('\r') == heredoc.delimiter {
                            break;
                        }
                        lines.push(line.to_string());
                    }
                    let body = lines.join("\n");
                    if !heredoc.literal {
                        nested.extend(substitutions(&body.chars().collect::<Vec<_>>(), 0));
                    }
                    bodies.push((heredoc.position, body));
                }
                for (position, body) in bodies.into_iter().rev() {
                    out.insert(position, Token::Input(body));
                }
                out.extend(nested.into_iter().map(Token::Nested));
            }
            '|' | '&' | ';' | '(' | ')' => {
                if c == '&' && next == Some('>') {
                    end_word(&mut word, &mut quoted, &mut out);
                    at += 1;
                    continue;
                }
                if c == '(' && next == Some('(') && word.is_empty() && !quoted {
                    let close = closing(&chars, at + 1, 1, ARITHMETIC);
                    if close < chars.len() && arithmetic(&chars[at + 1..close]) {
                        // `((..))` is arithmetic, not two subshells.
                        out.extend(
                            substitutions(&chars[at + 1..close], 0)
                                .into_iter()
                                .map(Token::Nested),
                        );
                        out.push(Token::Break);
                        at = close + 1;
                        continue;
                    }
                }
                end_word(&mut word, &mut quoted, &mut out);
                let doubled = next == Some(c) && (c == '|' || c == '&');
                out.push(match (c, doubled) {
                    ('|', false) => Token::Pipe,
                    ('&', true) => Token::And,
                    ('(', _) => Token::Open,
                    (')', _) => Token::Close,
                    _ => Token::Break,
                });
                // `|&` pipes both streams.
                if doubled || (c == '|' && next == Some('&')) {
                    at += 1;
                }
            }
            '>' => {
                // `2>` and `&>` name a stream, not a word.
                if word.chars().all(|c| c.is_ascii_digit()) && !quoted {
                    word.clear();
                } else {
                    end_word(&mut word, &mut quoted, &mut out);
                }
                if matches!(chars.get(at + 1), Some('>') | Some('|')) {
                    at += 1;
                }
                if chars.get(at + 1) == Some(&'&') {
                    // `>&2` and `>&-` duplicate or close a stream; `>& file`
                    // writes the file.
                    let mut start = at + 2;
                    while matches!(chars.get(start), Some(' ' | '\t')) {
                        start += 1;
                    }
                    let mut end = start;
                    while chars
                        .get(end)
                        .is_some_and(|c| c.is_ascii_digit() || *c == '-')
                    {
                        end += 1;
                    }
                    let stream =
                        end > start && chars.get(end).is_none_or(|c| " \t\n;|&<>()".contains(*c));
                    if stream {
                        at = end - 1;
                    } else {
                        at += 1;
                        out.push(Token::WriteTo);
                    }
                } else {
                    out.push(Token::WriteTo);
                }
            }
            '<' => {
                end_word(&mut word, &mut quoted, &mut out);
                match next {
                    Some('<') if chars.get(at + 2) == Some(&'<') => {
                        at += 2;
                        out.push(Token::HereString);
                    }
                    Some('<') => {
                        at += 2;
                        let strip_tabs = chars.get(at) == Some(&'-');
                        if strip_tabs {
                            at += 1;
                        }
                        while matches!(chars.get(at), Some(' ' | '\t')) {
                            at += 1;
                        }
                        // The delimiter is a word; quoting any of it keeps
                        // the body literal.
                        let mut delimiter = String::new();
                        let mut literal = false;
                        while at < chars.len() && !" \t\n;|&<>()".contains(chars[at]) {
                            match chars[at] {
                                quote @ ('\'' | '"') => {
                                    literal = true;
                                    at += 1;
                                    while at < chars.len() && chars[at] != quote {
                                        delimiter.push(chars[at]);
                                        at += 1;
                                    }
                                }
                                '\\' => {
                                    literal = true;
                                    at += 1;
                                    delimiter.extend(chars.get(at));
                                }
                                other => delimiter.push(other),
                            }
                            at += 1;
                        }
                        heredocs.push(Heredoc {
                            delimiter,
                            strip_tabs,
                            literal,
                            position: out.len(),
                        });
                        continue;
                    }
                    Some('(') => {
                        at += 1;
                        out.push(Token::Feeds);
                    }
                    // `<>` opens the file to read and write.
                    Some('>') => {
                        at += 1;
                        out.push(Token::WriteTo);
                    }
                    // `<&3` duplicates a stream.
                    Some('&') => {
                        at += 1;
                        while chars
                            .get(at + 1)
                            .is_some_and(|c| c.is_ascii_digit() || *c == '-')
                        {
                            at += 1;
                        }
                    }
                    _ => out.push(Token::ReadFrom),
                }
            }
            _ => word.push(c),
        }
        at += 1;
    }
    end_word(&mut word, &mut quoted, &mut out);
    out
}

fn is_word(token: &Token) -> bool {
    matches!(token, Token::Word(_))
}

/// How deep code handed to a shell as a string is read again; past it,
/// every word is taken for a path it may write.
const DEPTH: usize = 8;
/// How many of the commands `xargs -I` runs are read; past them, each item
/// is taken for a path it may write.
const RUNS: usize = 64;

/// Reserved words a command can follow: `if rm x`, `then rm x`, `! rm x`,
/// `{ rm x; }`.
const KEYWORDS: &[&str] = &[
    "if", "then", "elif", "else", "do", "while", "until", "!", "{", "coproc",
];
/// Programs that run the rest of their arguments as a command: the letters
/// and long names of their options that take a value, and how many
/// operands come before the command (`timeout 5 rm x`).
const WRAPPERS: &[(&str, &str, &[&str], usize)] = &[
    (
        "sudo",
        "ugpChDrtTUR",
        &[
            "user",
            "group",
            "prompt",
            "close-from",
            "chdir",
            "role",
            "type",
            "command-timeout",
            "other-user",
            "chroot",
            "host",
        ],
        0,
    ),
    ("doas", "uC", &[], 0),
    ("env", "uCS", &["unset", "chdir", "split-string"], 0),
    ("command", "", &[], 0),
    ("nohup", "", &[], 0),
    ("time", "fo", &["format", "output"], 0),
    ("nice", "n", &["adjustment"], 0),
    ("exec", "a", &[], 0),
    ("timeout", "ks", &["kill-after", "signal"], 1),
    ("stdbuf", "ioe", &["input", "output", "error"], 0),
    ("ionice", "cnp", &["class", "classdata", "pid"], 0),
    ("setsid", "", &[], 0),
    ("busybox", "", &[], 0),
    ("watch", "n", &["interval"], 0),
    (
        "xargs",
        "ILnPsdEa",
        &[
            "arg-file",
            "delimiter",
            "max-args",
            "max-procs",
            "max-chars",
            "max-lines",
            "process-slot-var",
        ],
        0,
    ),
];
const FETCHERS: &[&str] = &["curl", "wget"];
/// Shells, which run the string after `-c` or the commands on their input.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "mksh", "ash", "fish"];

/// An interpreter whose code this reader hands back for what it names,
/// and how its options give that code.
struct Language {
    /// Its names; each also with a version after it, as `python3.12`.
    names: &'static [&'static str],
    /// Options whose value is code, as `-c` or `--eval`.
    code: &'static [&'static str],
    /// Options whose value is the script, as php's `-f`.
    script: &'static [&'static str],
    /// Other options that take a value.
    valued: &'static [&'static str],
    /// Letters that take the rest of their word, however short, as perl's
    /// `-i.bak`.
    attached: &'static str,
    /// Whether a letter's value may be the rest of its word, as `-cCODE`;
    /// otherwise it is the next word.
    glued: bool,
    /// Options past which every word is the program's own, as python's `-c`
    /// and `-m`.
    ends: &'static [&'static str],
    /// Whether its first operand may be `eval`, whose next is code, or
    /// `run`, whose next is the script.
    subcommands: bool,
}

const LANGUAGES: &[Language] = &[
    Language {
        names: &["python", "pypy"],
        code: &["c"],
        script: &[],
        valued: &["W", "X", "check-hash-based-pycs"],
        attached: "",
        glued: true,
        ends: &["c", "m"],
        subcommands: false,
    },
    Language {
        names: &["node", "nodejs"],
        code: &["e", "p", "eval", "print"],
        script: &[],
        valued: &[
            "r",
            "require",
            "import",
            "loader",
            "experimental-loader",
            "C",
            "conditions",
            "input-type",
            "env-file",
            "title",
        ],
        attached: "",
        glued: false,
        ends: &[],
        subcommands: false,
    },
    Language {
        names: &["deno"],
        code: &["eval"],
        script: &[],
        valued: &["c", "config", "import-map", "lock", "cert", "env-file"],
        attached: "",
        glued: false,
        ends: &[],
        subcommands: true,
    },
    Language {
        names: &["bun"],
        code: &["e", "p", "eval", "print"],
        script: &[],
        valued: &["r", "preload", "cwd", "env-file", "config"],
        attached: "",
        glued: false,
        ends: &[],
        subcommands: true,
    },
    Language {
        names: &["perl"],
        code: &["e", "E"],
        script: &[],
        valued: &[],
        attached: "iIMmxdDC",
        glued: true,
        ends: &[],
        subcommands: false,
    },
    Language {
        names: &["ruby"],
        code: &["e"],
        script: &[],
        valued: &["r", "I", "C", "E", "F", "encoding"],
        attached: "ixWKT",
        glued: true,
        ends: &[],
        subcommands: false,
    },
    Language {
        names: &["php"],
        code: &["r", "B", "R", "E"],
        script: &["f"],
        valued: &["c", "d", "t", "S", "z"],
        attached: "",
        glued: true,
        ends: &[],
        subcommands: false,
    },
    Language {
        names: &["Rscript"],
        code: &["e"],
        script: &[],
        valued: &[],
        attached: "",
        glued: false,
        ends: &[],
        subcommands: false,
    },
    Language {
        names: &["lua", "luajit"],
        code: &["e"],
        script: &[],
        valued: &["l"],
        attached: "",
        glued: true,
        ends: &[],
        subcommands: false,
    },
    Language {
        names: &["osascript"],
        code: &["e"],
        script: &[],
        valued: &["l", "s"],
        attached: "",
        glued: false,
        ends: &[],
        subcommands: false,
    },
];

/// The interpreter a program is, by its name, if this reader knows it.
fn language(name: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|language| {
        language.names.iter().any(|known| {
            name.strip_prefix(known)
                .is_some_and(|version| version.chars().all(|c| c.is_ascii_digit() || c == '.'))
        })
    })
}

/// Whether a program runs the code it is given: a shell or an interpreter
/// this reader knows.
fn runs_code(name: &str) -> bool {
    SHELLS.contains(&name) || language(name).is_some()
}

const COMPRESSORS: &[&str] = &[
    "gzip",
    "gunzip",
    "bzip2",
    "bunzip2",
    "xz",
    "unxz",
    "lzma",
    "unlzma",
    "zstd",
    "unzstd",
    "compress",
    "uncompress",
    "lz4",
    "unlz4",
    "pigz",
    "unpigz",
    "brotli",
];

fn program(word: &str) -> &str {
    word.rsplit(['/', '\\']).next().unwrap_or(word)
}

/// A simple command from its program on.
struct Command<'a> {
    words: &'a [String],
    /// Run by `xargs`: with the string it replaces by each item, if any.
    xargs: Option<Option<String>>,
}

/// A simple command past reserved words, variable assignments and
/// wrappers such as `sudo -E env A=1`, with their options.
fn command(words: &[String]) -> Command<'_> {
    let mut at = 0;
    while let Some(word) = words.get(at) {
        if word == "function" {
            // `function f { rm x; }` runs from its body.
            match words[at..].iter().position(|word| word == "{") {
                Some(open) => at += open + 1,
                None => at = words.len(),
            }
        } else if KEYWORDS.contains(&word.as_str()) {
            at += 1;
        } else {
            break;
        }
    }
    let mut xargs = None;
    while let Some(word) = words.get(at) {
        if word.contains('=') && !word.starts_with('-') {
            at += 1;
            continue;
        }
        let name = program(word);
        let Some((_, short, long, mut operands)) = WRAPPERS
            .iter()
            .copied()
            .find(|(wrapper, ..)| *wrapper == name)
        else {
            break;
        };
        at += 1;
        let mut replace = None;
        while let Some(arg) = words.get(at) {
            if arg == "--" {
                at += 1;
                break;
            }
            if arg.starts_with('-') {
                if name == "xargs" {
                    replace = replace.or_else(|| xargs_replace(arg, words.get(at + 1)));
                }
                // `command -v x` says what `x` is, and runs nothing.
                if name == "command" && arg.trim_start_matches('-').contains(['v', 'V']) {
                    return Command {
                        words: &[],
                        xargs: None,
                    };
                }
                at += 1 + usize::from(takes_value(arg, short, long));
            } else if operands > 0 {
                operands -= 1;
                at += 1;
            } else {
                break;
            }
        }
        if name == "xargs" {
            xargs = Some(replace);
        }
    }
    Command {
        words: words.get(at..).unwrap_or_default(),
        xargs,
    }
}

/// Whether option `arg` takes the next word as its value: a long name in
/// `long` written without `=`, or letters whose last is one in `short`.
fn takes_value(arg: &str, short: &str, long: &[&str]) -> bool {
    if let Some(name) = arg.strip_prefix("--") {
        return long.contains(&name);
    }
    for (at, letter) in arg.char_indices().skip(1) {
        if short.contains(letter) {
            return at + letter.len_utf8() == arg.len();
        }
    }
    false
}

/// The string `xargs -I R`, `-i` or `--replace` replaces by each item.
fn xargs_replace(arg: &str, next: Option<&String>) -> Option<String> {
    if let Some(long) = arg.strip_prefix("--") {
        return match long.split_once('=') {
            Some(("replace", replace)) => Some(replace.to_string()),
            None if long == "replace" => Some("{}".to_string()),
            _ => None,
        };
    }
    for (at, letter) in arg.char_indices().skip(1) {
        let rest = &arg[at + letter.len_utf8()..];
        match letter {
            'I' if rest.is_empty() => return next.cloned(),
            'i' if rest.is_empty() => return Some("{}".to_string()),
            'I' | 'i' => return Some(rest.to_string()),
            _ if "LnPsdEa".contains(letter) => return None,
            _ => {}
        }
    }
    None
}

/// Options and operands, getopt-style: see [`parse`].
struct Parsed<'a> {
    options: Vec<(&'a str, Option<&'a str>)>,
    operands: Vec<&'a str>,
    /// Where in `operands` those after `--` start.
    dashes: Option<usize>,
}

impl<'a> Parsed<'a> {
    fn has(&self, names: &[&str]) -> bool {
        self.options.iter().any(|(name, _)| names.contains(name))
    }

    fn values(&self, names: &[&str]) -> Vec<&'a str> {
        self.options
            .iter()
            .filter(|(name, _)| names.contains(name))
            .filter_map(|(_, value)| *value)
            .collect()
    }
}

/// Split `args` the way getopt does: a letter in `short` or a long name in
/// `long` takes a value, the rest of its word or the next word; options
/// may follow operands, and `--` ends them.
fn parse<'a>(args: &'a [String], short: &str, long: &[&str]) -> Parsed<'a> {
    let mut parsed = Parsed {
        options: Vec::new(),
        operands: Vec::new(),
        dashes: None,
    };
    let mut words = args.iter().map(String::as_str);
    while let Some(arg) = words.next() {
        if arg == "--" {
            parsed.dashes = Some(parsed.operands.len());
            parsed.operands.extend(words.by_ref());
            break;
        }
        if let Some(name) = arg.strip_prefix("--") {
            let option = match name.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None if long.contains(&name) => (name, words.next()),
                None => (name, None),
            };
            parsed.options.push(option);
        } else if arg.len() > 1 && arg.starts_with('-') {
            let letters = &arg[1..];
            for (at, letter) in letters.char_indices() {
                let end = at + letter.len_utf8();
                if short.contains(letter) {
                    let rest = &letters[end..];
                    let value = if rest.is_empty() {
                        words.next()
                    } else {
                        Some(rest)
                    };
                    parsed.options.push((&letters[at..end], value));
                    break;
                }
                parsed.options.push((&letters[at..end], None));
            }
        } else {
            parsed.operands.push(arg);
        }
    }
    parsed
}

fn strings(values: Vec<&str>) -> Vec<String> {
    values.into_iter().map(str::to_string).collect()
}

/// What one simple command of `words` changes and runs, `depth` strings
/// deep.
pub(crate) fn changed_by(words: &[String], depth: usize) -> Reading {
    changed(words, Input::Unknown, depth, &Variables::default())
}

fn merge(into: &mut Reading, found: Reading) {
    into.writes.extend(found.writes);
    if into.remote_code.is_none() {
        into.remote_code = found.remote_code;
    }
    into.code.extend(found.code);
    into.scripts.extend(found.scripts);
    into.unread.extend(found.unread);
    into.named.extend(found.named);
}

/// `word` as it is shown, with what a command substituted in it prints as
/// `$(..)`.
pub fn shown(word: &str) -> String {
    word.replace(BUILT, "$(..)")
}

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
/// read again, `depth` strings deep, with `vars` the variables it sees.
fn code_of(program: &str, code: &str, depth: usize, vars: &Variables) -> Reading {
    if code.contains(BUILT) {
        built(program)
    } else {
        read_in(code, depth + 1, vars)
    }
}

/// The script `program` runs at `path`, for the caller to read.
fn script_of(program: &str, path: &str, runner: Runner, depth: usize) -> Reading {
    if path.contains(BUILT) {
        return built(program);
    }
    Reading {
        scripts: vec![Script {
            path: path.to_string(),
            dir: String::new(),
            runner,
            depth,
            text: None,
        }],
        ..Reading::default()
    }
}

/// What one simple command of `words` changes and runs, `depth` strings
/// deep, given what it reads on its input and the variables the line set.
fn changed(words: &[String], input: Input, depth: usize, vars: &Variables) -> Reading {
    if depth > DEPTH {
        return Reading {
            writes: words.to_vec(),
            ..Reading::default()
        };
    }
    let found = command(words);
    if let Some(replace) = found.xargs {
        return xargs(found.words, replace.as_deref(), input.text(), depth, vars);
    }
    let Some((name, args)) = found.words.split_first() else {
        return Reading::default();
    };
    // A program whose name is built when the line runs may be any: every
    // word it is given may be a path it writes.
    if program(name).contains(BUILT) {
        return Reading {
            writes: args.to_vec(),
            ..Reading::default()
        };
    }
    match program(name) {
        "eval" => code_of("eval", &args.join(" "), depth, vars),
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
            let mut reading = Reading::default();
            for code in parsed.values(&["c", "command", "session-command"]) {
                merge(
                    &mut reading,
                    code_of("su", code, depth, &Variables::default()),
                );
            }
            reading
        }
        "find" => find(args, depth, vars),
        "." | "source" => match args.first() {
            Some(path) => script_of(program(name), path, Runner::Shell, depth),
            None => Reading::default(),
        },
        "pwsh" | "powershell" => super::powershell::invoked(args, depth),
        shell if SHELLS.contains(&shell) => match shell_code(args, input) {
            Given::Code(code) => code_of(shell, &code, depth, vars),
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
                        // Code that is all built when the line runs may be
                        // any; code with a name built in it is judged by
                        // what it spells.
                        Given::Code(text)
                            if text.chars().all(|c| c == BUILT || c.is_whitespace()) =>
                        {
                            built(program_name)
                        }
                        Given::Code(text) => Reading {
                            code: vec![Code {
                                program: program_name.to_string(),
                                text: text.replace(BUILT, " "),
                                dir: String::new(),
                            }],
                            ..Reading::default()
                        },
                        // A script whose path is built when the line runs
                        // is no more known than any program's code.
                        Given::Script(path) if path.contains(BUILT) => Reading::default(),
                        Given::Script(path) => script_of(program_name, &path, runner, depth),
                        Given::Nothing => Reading::default(),
                    },
                );
            } else if reading.writes.is_empty()
                && program_name.len() < name.len()
                && !name.contains(BUILT)
            {
                // A program named by its path may be a script. One in a
                // directory built when the line runs is no shell this
                // reader knows, and is left unread.
                merge(
                    &mut reading,
                    script_of(program_name, name, Runner::Program, depth),
                );
            }
            reading
        }
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
) -> Reading {
    let mut reading = Reading::default();
    if words.is_empty() {
        return reading;
    }
    let items: Vec<&str> = input
        .map(|text| text.split_whitespace().collect())
        .unwrap_or_default();
    let runs: Vec<Vec<String>> = match replace.filter(|replace| !replace.is_empty()) {
        Some(replace) if !items.is_empty() => {
            reading
                .writes
                .extend(items.iter().skip(RUNS).map(|item| item.to_string()));
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
        merge(&mut reading, changed(&run, Input::Unknown, depth + 1, vars));
    }
    reading
}

/// What a command reads on its input, as far as the line spells it.
#[derive(Clone, Copy)]
enum Input<'a> {
    /// Nothing the line spells.
    Unknown,
    /// Text: a here-document, a here-string, or what `echo` pipes.
    Text(&'a str),
    /// A file redirected into it with `<`.
    File(&'a str),
}

impl<'a> Input<'a> {
    fn text(self) -> Option<&'a str> {
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
fn find(args: &[String], depth: usize, vars: &Variables) -> Reading {
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
                let mut found = changed(&args[at + 1..end], Input::Unknown, depth + 1, vars);
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

/// The paths a program changes through its arguments.
fn written(name: &str, args: &[String]) -> Vec<String> {
    let options_end = args.iter().position(|arg| arg == "--");
    let operands: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(index, arg)| options_end.is_some_and(|end| *index > end) || !arg.starts_with('-'))
        .filter(|(index, _)| options_end != Some(*index))
        .map(|(_, arg)| arg.clone())
        .collect();
    // `-i`, `-i.bak`, `--in-place`, yq's `--inplace`, and perl's `-pi -e`.
    let in_place = args.iter().any(|arg| {
        arg.starts_with("-i")
            || arg.starts_with("--in-place")
            || arg == "--inplace"
            || (!arg.starts_with("--") && arg.starts_with('-') && arg.contains('i'))
    });
    match name {
        "rm" | "unlink" | "shred" | "truncate" | "touch" | "chmod" | "tee" | "rmdir" => operands,
        "cp" | "mv" | "ln" | "install" => copied(name, args),
        "sed" | "perl" | "ruby" | "yq" if in_place => operands,
        // gawk's `-i inplace`.
        "awk" | "gawk"
            if args.iter().enumerate().any(|(at, arg)| {
                let library = match arg.as_str() {
                    "-i" | "--include" => args.get(at + 1).map(String::as_str),
                    _ => arg
                        .strip_prefix("--include=")
                        .or_else(|| arg.strip_prefix("-i")),
                };
                library.is_some_and(|library| library.starts_with("inplace"))
            }) =>
        {
            operands
        }
        "dd" => args
            .iter()
            .filter_map(|arg| arg.strip_prefix("of="))
            .map(str::to_string)
            .collect(),
        "git" => git(args),
        "curl" => curl(args),
        "wget" => wget(args),
        "patch" => patch(args),
        // Its last operand is written, and pruned with `--delete`.
        "rsync" => {
            let parsed = parse(
                args,
                "efBTM@",
                &[
                    "exclude",
                    "include",
                    "filter",
                    "files-from",
                    "exclude-from",
                    "include-from",
                    "rsh",
                    "temp-dir",
                    "partial-dir",
                    "backup-dir",
                    "suffix",
                    "chmod",
                    "chown",
                    "log-file",
                    "password-file",
                    "link-dest",
                    "copy-dest",
                    "compare-dest",
                    "max-size",
                    "min-size",
                    "bwlimit",
                    "timeout",
                    "port",
                    "out-format",
                    "rsync-path",
                ],
            );
            if parsed.has(&["n", "dry-run", "list-only"]) || parsed.operands.len() < 2 {
                return Vec::new();
            }
            strings(parsed.operands[parsed.operands.len() - 1..].to_vec())
        }
        "tar" => tar(args),
        // It extracts into `-d DIR` or here.
        "unzip" => {
            let parsed = parse(args, "dPx", &[]);
            if parsed.has(&["l", "t", "v", "p", "c", "z", "Z"]) {
                return Vec::new();
            }
            vec![parsed
                .values(&["d"])
                .last()
                .copied()
                .unwrap_or(".")
                .to_string()]
        }
        "sort" => strings(
            parse(
                args,
                "okStT",
                &[
                    "output",
                    "key",
                    "buffer-size",
                    "field-separator",
                    "temporary-directory",
                    "files0-from",
                    "compress-program",
                    "parallel",
                    "batch-size",
                    "random-source",
                ],
            )
            .values(&["o", "output"]),
        ),
        name if COMPRESSORS.contains(&name) => compressed(args),
        _ => Vec::new(),
    }
}

/// What `cp`, `mv`, `ln` or `install` writes: the target when it may be the
/// file written, each source's name in it when it may be a directory, and
/// what `mv` takes away. A trailing `/`, `.`, `-t DIR` or several sources
/// make the target a directory.
fn copied(name: &str, args: &[String]) -> Vec<String> {
    let parsed = parse(
        args,
        if name == "install" { "tSmog" } else { "tS" },
        &[
            "target-directory",
            "suffix",
            "mode",
            "owner",
            "group",
            "strip-program",
        ],
    );
    if name == "install" && parsed.has(&["d", "directory"]) {
        return strings(parsed.operands);
    }
    let (target, sources, directory) = match parsed.values(&["t", "target-directory"]).last() {
        Some(dir) => (*dir, &parsed.operands[..], true),
        None => {
            let Some((target, sources)) = parsed.operands.split_last() else {
                return Vec::new();
            };
            if sources.is_empty() {
                // `ln -s x` links `x` here; a glob or brace may stand for
                // the sources and the target.
                let mut written = Vec::new();
                if name == "ln" {
                    written.push(base(target).to_string());
                }
                if name == "mv" || target.contains(['*', '?', '[', '{']) {
                    written.push(target.to_string());
                }
                return written;
            }
            let directory =
                target.ends_with('/') || matches!(*target, "." | "..") || sources.len() > 1;
            (*target, sources, directory)
        }
    };
    let mut written = if name == "mv" {
        strings(sources.to_vec())
    } else {
        Vec::new()
    };
    if parsed.has(&["T", "no-target-directory"]) {
        written.push(target.to_string());
        return written;
    }
    if !directory {
        written.push(target.to_string());
    }
    written.extend(sources.iter().map(|source| joined(target, base(source))));
    written
}

/// What a `git` command changes, under its `-C`: its paths, `.` for the
/// whole tree when it rewrites the working tree, or its configuration file.
fn git(args: &[String]) -> Vec<String> {
    let mut dir = String::new();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        match arg.as_str() {
            "-C" => {
                if let Some(next) = args.get(at + 1) {
                    dir = located(Some(&dir), next);
                }
                at += 2;
            }
            "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix"
            | "--config-env" | "--attr-source" => at += 2,
            option if option.starts_with('-') => at += 1,
            _ => break,
        }
    }
    let Some((sub, rest)) = args.get(at..).and_then(<[String]>::split_first) else {
        return Vec::new();
    };
    let whole = || vec![".".to_string()];
    let paths_or_whole = |parsed: Parsed| {
        if parsed.operands.is_empty() {
            whole()
        } else {
            strings(parsed.operands)
        }
    };
    let written = match sub.as_str() {
        "checkout" => {
            let parsed = parse(rest, "bB", &["orphan", "conflict", "pathspec-from-file"]);
            match parsed.dashes {
                Some(start) => strings(parsed.operands[start..].to_vec()),
                None if parsed.has(&["pathspec-from-file"]) => whole(),
                // A revision and paths, or paths alone.
                None if parsed.operands.len() > 1 => strings(parsed.operands),
                // A revision: the tree changes to it.
                None if !parsed.operands.is_empty()
                    || parsed.has(&["f", "force", "m", "merge", "p", "patch"]) =>
                {
                    whole()
                }
                None => Vec::new(),
            }
        }
        "restore" => paths_or_whole(parse(rest, "s", &["source", "pathspec-from-file"])),
        "switch" => {
            let parsed = parse(
                rest,
                "cC",
                &["create", "force-create", "orphan", "conflict"],
            );
            if parsed.operands.is_empty() && parsed.has(&["c", "C", "create", "force-create"]) {
                Vec::new()
            } else {
                whole()
            }
        }
        "stash" => match rest.first().map(String::as_str) {
            Some("list" | "show" | "drop" | "clear" | "create" | "store") => Vec::new(),
            Some("push") => {
                paths_or_whole(parse(&rest[1..], "m", &["message", "pathspec-from-file"]))
            }
            Some(option) if option.starts_with('-') => {
                paths_or_whole(parse(rest, "m", &["message", "pathspec-from-file"]))
            }
            _ => whole(),
        },
        "reset"
            if rest
                .iter()
                .any(|arg| matches!(arg.as_str(), "--hard" | "--merge" | "--keep")) =>
        {
            whole()
        }
        "apply" => {
            let parsed = parse(rest, "pC", &["directory", "exclude", "include"]);
            let only_reads =
                parsed.has(&["check", "stat", "numstat", "summary"]) && !parsed.has(&["apply"]);
            if only_reads || parsed.has(&["cached"]) {
                Vec::new()
            } else {
                whole()
            }
        }
        // Commits brought into the tree, which may change any file; only
        // stopping or resuming one already judged does not.
        "merge" | "pull" | "rebase" | "cherry-pick" | "revert" | "am" => {
            if rest.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "--abort" | "--continue" | "--quit" | "--skip" | "--show-current-patch"
                )
            }) {
                Vec::new()
            } else {
                whole()
            }
        }
        "rm" | "mv" => strings(parse(rest, "", &["pathspec-from-file"]).operands),
        "clean" => {
            let parsed = parse(rest, "e", &["exclude"]);
            if parsed.has(&["n", "dry-run"]) {
                Vec::new()
            } else {
                paths_or_whole(parsed)
            }
        }
        "config" => config(rest),
        _ => Vec::new(),
    };
    written
        .into_iter()
        .map(|path| located(Some(&dir), &path))
        .collect()
}

/// The file `git config` writes, when it sets or removes something.
fn config(args: &[String]) -> Vec<String> {
    let parsed = parse(
        args,
        "f",
        &["file", "blob", "type", "default", "comment", "value"],
    );
    let first = parsed.operands.first().copied();
    let reads = parsed.has(&[
        "get",
        "get-all",
        "get-regexp",
        "get-urlmatch",
        "get-color",
        "get-colorbool",
        "list",
        "l",
        "blob",
    ]) || matches!(first, Some("get" | "list"));
    let writes = parsed.has(&[
        "unset",
        "unset-all",
        "add",
        "replace-all",
        "rename-section",
        "remove-section",
        "edit",
        "e",
    ]) || matches!(
        first,
        Some("set" | "unset" | "rename-section" | "remove-section" | "edit")
    ) || parsed.operands.len() > 1;
    if reads || !writes {
        return Vec::new();
    }
    let file = match parsed.values(&["f", "file"]).last() {
        Some(file) => file,
        None if parsed.has(&["global"]) => "~/.gitconfig",
        None if parsed.has(&["system"]) => "/etc/gitconfig",
        None if parsed.has(&["worktree"]) => ".git/config.worktree",
        None => ".git/config",
    };
    vec![file.to_string()]
}

/// The file name a URL is saved under: its path's last segment.
fn url_name(url: &str) -> Option<&str> {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    let path = url.split_once("://").map_or(url, |(_, rest)| rest);
    let name = path.rsplit_once('/')?.1;
    (!name.is_empty()).then_some(name)
}

/// What `curl` saves: `-o`, `-D`, `-c` and the like, or with `-O` each
/// URL's file name; under `--output-dir`.
fn curl(args: &[String]) -> Vec<String> {
    let parsed = parse(
        args,
        "AbcCdDeEFHKmorTuUwxXyYzQPt",
        &[
            "output",
            "dump-header",
            "cookie-jar",
            "cookie",
            "data",
            "data-raw",
            "data-binary",
            "data-urlencode",
            "form",
            "header",
            "user-agent",
            "user",
            "proxy",
            "request",
            "referer",
            "url",
            "max-time",
            "connect-timeout",
            "retry",
            "config",
            "cert",
            "key",
            "cacert",
            "output-dir",
            "write-out",
            "trace",
            "trace-ascii",
            "stderr",
            "libcurl",
            "etag-save",
            "range",
            "upload-file",
            "time-cond",
            "resolve",
            "limit-rate",
            "json",
        ],
    );
    let dir = parsed
        .values(&["output-dir"])
        .last()
        .copied()
        .unwrap_or_default();
    let mut written: Vec<String> = parsed
        .values(&["o", "output"])
        .into_iter()
        .map(|path| located(Some(dir), path))
        .collect();
    written.extend(strings(parsed.values(&[
        "D",
        "dump-header",
        "c",
        "cookie-jar",
        "trace",
        "trace-ascii",
        "stderr",
        "libcurl",
        "etag-save",
    ])));
    if parsed.has(&["O", "remote-name", "remote-name-all"]) {
        let urls = parsed
            .operands
            .iter()
            .copied()
            .chain(parsed.values(&["url"]));
        written.extend(
            urls.filter_map(url_name)
                .map(|name| located(Some(dir), name)),
        );
    }
    written
}

/// What `wget` saves: `-O`, its logs, or each URL's file name under `-P`.
fn wget(args: &[String]) -> Vec<String> {
    let parsed = parse(
        args,
        "OoaPeiBtTwQUDlARIXn",
        &[
            "output-document",
            "output-file",
            "append-output",
            "directory-prefix",
            "execute",
            "input-file",
            "base",
            "tries",
            "timeout",
            "wait",
            "quota",
            "user-agent",
            "domains",
            "level",
            "accept",
            "reject",
            "header",
            "user",
            "password",
            "post-data",
            "post-file",
            "referer",
            "save-cookies",
            "load-cookies",
        ],
    );
    let mut written =
        strings(parsed.values(&["o", "output-file", "a", "append-output", "save-cookies"]));
    if parsed.has(&["spider"]) {
        return written;
    }
    let documents = parsed.values(&["O", "output-document"]);
    if !documents.is_empty() {
        written.extend(strings(documents));
        return written;
    }
    let dir = parsed
        .values(&["P", "directory-prefix"])
        .last()
        .copied()
        .unwrap_or_default();
    if parsed.has(&["r", "recursive", "m", "mirror"]) {
        written.push(if dir.is_empty() { "." } else { dir }.to_string());
    } else {
        written.extend(
            parsed
                .operands
                .iter()
                .map(|url| located(Some(dir), url_name(url).unwrap_or("index.html"))),
        );
    }
    written
}

/// What `patch` changes: the file it is given, its `-o` and `-r` files,
/// or, when the patch names the files, the whole tree under `-d`.
fn patch(args: &[String]) -> Vec<String> {
    let parsed = parse(
        args,
        "BdDFgioprVYz",
        &[
            "directory",
            "input",
            "output",
            "reject-file",
            "strip",
            "fuzz",
            "prefix",
            "suffix",
            "basename-prefix",
            "ifdef",
            "get",
            "version-control",
            "quoting-style",
            "reject-format",
        ],
    );
    if parsed.has(&["dry-run"]) {
        return Vec::new();
    }
    let dir = parsed
        .values(&["d", "directory"])
        .last()
        .copied()
        .unwrap_or_default();
    let mut written: Vec<String> = parsed
        .values(&["o", "output", "r", "reject-file"])
        .into_iter()
        .map(|path| located(Some(dir), path))
        .collect();
    if parsed.has(&["o", "output"]) {
        return written;
    }
    written.push(match parsed.operands.first() {
        Some(file) => located(Some(dir), file),
        None if dir.is_empty() => ".".to_string(),
        None => dir.to_string(),
    });
    written
}

/// What `tar` changes: the directory it extracts into, or the archive it
/// creates or appends to.
fn tar(args: &[String]) -> Vec<String> {
    let mut args = args.to_vec();
    // `tar xzf a.tgz`: old-style letters without a dash.
    if let Some(first) = args.first_mut() {
        if !first.is_empty() && first.chars().all(|c| c.is_ascii_alphabetic()) {
            first.insert(0, '-');
        }
    }
    let parsed = parse(
        &args,
        "fCbFgHKLNTVXI",
        &[
            "file",
            "directory",
            "blocking-factor",
            "info-script",
            "new-volume-script",
            "listed-incremental",
            "format",
            "starting-file",
            "tape-length",
            "newer",
            "newer-mtime",
            "files-from",
            "label",
            "exclude-from",
            "use-compress-program",
            "exclude",
            "transform",
            "owner",
            "group",
            "mode",
            "mtime",
            "suffix",
            "to-command",
            "strip-components",
        ],
    );
    if parsed.has(&["x", "extract", "get"]) {
        if parsed.has(&["O", "to-stdout", "t", "list"]) {
            return Vec::new();
        }
        let dir = parsed.values(&["C", "directory"]).last().copied();
        return vec![dir.unwrap_or(".").to_string()];
    }
    if parsed.has(&[
        "c",
        "r",
        "u",
        "A",
        "create",
        "append",
        "update",
        "catenate",
        "concatenate",
        "delete",
    ]) {
        return strings(parsed.values(&["f", "file"]));
    }
    Vec::new()
}

/// What a compressor replaces: each file, and the name it decompresses to;
/// nothing but `-o` when it writes to its output, lists or tests.
fn compressed(args: &[String]) -> Vec<String> {
    let parsed = parse(
        args,
        "SToDFCMo",
        &["suffix", "threads", "format", "check", "memlimit", "output"],
    );
    let mut written = strings(parsed.values(&["o", "output"]));
    if parsed.has(&["c", "stdout", "to-stdout", "l", "list", "t", "test"]) {
        return written;
    }
    for file in &parsed.operands {
        written.push(file.to_string());
        let stem = [
            ".gz", ".bz2", ".xz", ".lzma", ".zst", ".lz4", ".br", ".Z", ".z",
        ]
        .iter()
        .find_map(|suffix| file.strip_suffix(suffix));
        written.extend(stem.map(str::to_string));
    }
    written
}

/// The last name in `path`.
fn base(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.rsplit('/').next().unwrap_or(path)
}

/// `path` in the directory `dir`, as written; an empty `dir` or `.` is here.
pub(crate) fn joined(dir: &str, path: &str) -> String {
    if dir.is_empty() || dir == "." {
        return path.to_string();
    }
    let dir = dir.trim_end_matches('/');
    match path.trim_start_matches("./") {
        "" | "." if dir.is_empty() => "/".to_string(),
        "" | "." => dir.to_string(),
        path => format!("{dir}/{path}"),
    }
}

/// `path` with its `.` and each `..` after a name taken out, as text: how
/// two spellings of one path are told to be the same.
fn lexical(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "." => {}
            ".." if parts
                .last()
                .is_some_and(|last| !last.is_empty() && *last != "..") =>
            {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

/// `path` as a command run in `dir` names it, `dir` relative to where the
/// line starts: an absolute path, `~`, a variable other than `$PWD` or a
/// start built when the line runs stays as written, and so does every path
/// when `dir` is not known.
pub(crate) fn located(dir: Option<&str>, path: &str) -> String {
    let Some(dir) = dir.filter(|dir| !dir.is_empty()) else {
        return path.to_string();
    };
    for pwd in ["$PWD", "${PWD}"] {
        if let Some(rest) = path
            .strip_prefix(pwd)
            .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        {
            return joined(dir, rest.trim_start_matches('/'));
        }
    }
    if path.starts_with(['/', '~', '$', BUILT]) {
        path.to_string()
    } else {
        joined(dir, path)
    }
}

/// Moves `dirs` past `words` when it is a `cd`: to the directory it names,
/// or to one this reader cannot name (`cd`, `cd -`, `cd $X`). Only after
/// `cd x &&` do the commands surely run there; otherwise they may run in
/// either place.
fn moved(dirs: &mut Vec<Option<String>>, words: &[String], certain: bool) {
    let Some((name, args)) = command(words).words.split_first() else {
        return;
    };
    let target = match program(name) {
        "cd" | "pushd" => args
            .iter()
            .map(String::as_str)
            .find(|arg| *arg == "-" || !arg.starts_with('-'))
            .filter(|dir| *dir != "-" && !dir.contains(['$', '`', BUILT])),
        "popd" => None,
        _ => return,
    };
    let next: Vec<Option<String>> = dirs
        .iter()
        .map(|dir| {
            let target = target?;
            if target.starts_with(['/', '~']) {
                Some(target.to_string())
            } else {
                dir.as_deref().map(|dir| joined(dir, target))
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

/// What a simple command writes to its output when the line spells it:
/// `echo` and `printf` their words, `cat` with no file its input.
fn output_of(words: &[String], input: Input) -> Option<String> {
    let (name, args) = command(words).words.split_first()?;
    let text = match program(name) {
        "echo" => args
            .iter()
            .skip_while(|arg| {
                arg.len() > 1 && arg.starts_with('-') && arg[1..].chars().all(|c| "neE".contains(c))
            })
            .cloned()
            .collect::<Vec<_>>()
            .join(" "),
        "printf" => match args.split_first() {
            Some((format, values)) => printed(format, values),
            None => String::new(),
        },
        "cat" if args.iter().all(|arg| arg == "-") => return input.text().map(str::to_string),
        _ => return None,
    };
    Some(text.replace("\\n", "\n"))
}

/// What `printf format values..` prints: each `%s`, `%b`, `%d`, `%i` or
/// `%q` in `format` given the next value, and `format` again while values
/// are left; `%%` is `%`. Widths and flags are not read.
fn printed(format: &str, values: &[String]) -> String {
    let mut out = String::new();
    let mut values = values.iter();
    loop {
        let mut took = false;
        let mut chars = format.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('%') => out.push('%'),
                Some('s' | 'b' | 'd' | 'i' | 'q') => {
                    took = true;
                    out.extend(values.next().map(String::as_str));
                }
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            }
        }
        if !took || values.len() == 0 {
            return out;
        }
    }
}

/// The program a simple command runs, past assignments and wrappers.
fn program_of(words: &[String]) -> Option<String> {
    command(words)
        .words
        .first()
        .map(|word| program(word).to_string())
}

/// Whether `name` can name a shell variable.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The name and value of `NAME=value` or `NAME+=value`, the name with its
/// `+`.
fn assignment(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    is_name(name.strip_suffix('+').unwrap_or(name)).then_some((name, value))
}

/// The variables the line has set: each name with the values it may hold.
/// Whether one is exported is not told apart: a `$name` is put in wherever
/// the line spells it, in a string another shell runs too, so `f=x; sh -c
/// 'rm $f'` is read as removing `x`. Seeing more than runs is the safe way
/// to be wrong.
#[derive(Debug, Clone, Default)]
struct Variables(Vec<(String, Vec<String>)>);

impl Variables {
    fn values(&self, name: &str) -> Option<&[String]> {
        self.0
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, values)| values.as_slice())
    }

    /// Sets `name` (with a `+`, appends to it) to each of the values `value`
    /// may stand for.
    fn set(&mut self, name: &str, value: &str) {
        let values = self.substituted(value);
        let (name, values) = match name.strip_suffix('+') {
            Some(name) => {
                let before = self.values(name).unwrap_or(&[]).to_vec();
                let before = if before.is_empty() {
                    vec![String::new()]
                } else {
                    before
                };
                let joined = before
                    .iter()
                    .flat_map(|old| values.iter().map(move |new| format!("{old}{new}")))
                    .take(RUNS)
                    .collect();
                (name, joined)
            }
            None => (name, values),
        };
        self.put(name, values);
    }

    fn put(&mut self, name: &str, values: Vec<String>) {
        match self.0.iter_mut().find(|(known, _)| known == name) {
            Some(known) => known.1 = values,
            None => self.0.push((name.to_string(), values)),
        }
    }

    /// `word` with each `$NAME` and `${NAME}` the line set put in: one text
    /// for each value they may hold, as many as [`RUNS`]. Any other
    /// variable stays as written.
    fn substituted(&self, word: &str) -> Vec<String> {
        if self.0.is_empty() || !word.contains('$') {
            return vec![word.to_string()];
        }
        let mut done = vec![String::new()];
        let mut rest = word;
        while let Some(at) = rest.find('$') {
            for text in &mut done {
                text.push_str(&rest[..at]);
            }
            let after = &rest[at + 1..];
            let (name, used) = match after.strip_prefix('{') {
                Some(inner) => match inner.find('}') {
                    Some(end) => (&inner[..end], end + 2),
                    None => ("", 0),
                },
                None => {
                    let end = after
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .unwrap_or(after.len());
                    (&after[..end], end)
                }
            };
            match self.values(name) {
                Some(values) => {
                    done = done
                        .iter()
                        .flat_map(|text| values.iter().map(move |value| format!("{text}{value}")))
                        .take(RUNS)
                        .collect();
                    rest = &after[used..];
                }
                None => {
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

    /// The words `word` may run as: itself, or each text it stands for,
    /// and that text split at whitespace too when a value put whitespace in
    /// it, as an unquoted variable is split.
    fn choices(&self, word: &str) -> Vec<Vec<String>> {
        let mut choices = Vec::new();
        for text in self.substituted(word) {
            if text != word && text.contains(char::is_whitespace) {
                choices.push(text.split_whitespace().map(str::to_string).collect());
            }
            choices.push(vec![text]);
        }
        choices
    }

    /// The simple commands `words` may run as: every combination of the
    /// choices of its words, as many as [`RUNS`]; past them, runs in which
    /// each choice of each word is taken once.
    fn runs(&self, words: &[String]) -> Vec<Vec<String>> {
        let choices: Vec<Vec<Vec<String>>> = words.iter().map(|word| self.choices(word)).collect();
        let combinations = choices
            .iter()
            .try_fold(1usize, |count, choice| count.checked_mul(choice.len()))
            .filter(|count| *count <= RUNS);
        if combinations.is_none() {
            let most = choices.iter().map(Vec::len).max().unwrap_or(1);
            return (0..most)
                .map(|at| {
                    choices
                        .iter()
                        .flat_map(|choice| choice[at % choice.len()].clone())
                        .collect()
                })
                .collect();
        }
        let mut runs: Vec<Vec<String>> = vec![Vec::new()];
        for choice in &choices {
            runs = runs
                .iter()
                .flat_map(|run| {
                    choice.iter().map(move |words| {
                        let mut run = run.clone();
                        run.extend(words.iter().cloned());
                        run
                    })
                })
                .collect();
        }
        runs
    }

    /// Records what the simple command `words` sets: `NAME=value` alone, or
    /// after `export`, `readonly`, `local`, `declare` or `typeset`; and the
    /// words a `for NAME in` loop goes through. An assignment before a
    /// program is that program's environment, not the line's.
    fn assign(&mut self, words: &[String]) {
        let start = words
            .iter()
            .position(|word| !KEYWORDS.contains(&word.as_str()))
            .unwrap_or(words.len());
        let words = &words[start..];
        match words.first().map(String::as_str) {
            Some("for") => {
                if let (Some(name), Some("in")) = (words.get(1), words.get(2).map(String::as_str)) {
                    if is_name(name) {
                        let values = words[3..]
                            .iter()
                            .flat_map(|word| self.choices(word).into_iter().flatten())
                            .take(RUNS)
                            .collect();
                        self.put(name, values);
                    }
                }
            }
            Some("export" | "readonly" | "local" | "declare" | "typeset") => {
                for (name, value) in words[1..].iter().filter_map(|word| assignment(word)) {
                    self.set(name, value);
                }
            }
            Some(_) if words.iter().all(|word| assignment(word).is_some()) => {
                for (name, value) in words.iter().filter_map(|word| assignment(word)) {
                    self.set(name, value);
                }
            }
            _ => {}
        }
    }
}

/// Read `command` for the writes and downloads it names.
pub fn read(command: &str) -> Reading {
    read_at(command, 0)
}

/// Read `command`, code handed over as a string `depth` times. A script's
/// text is read one deeper than the code that runs it, so past [`DEPTH`],
/// where every word is taken for a path it may write, counts both.
pub fn read_at(command: &str, depth: usize) -> Reading {
    read_in(command, depth, &Variables::default())
}

/// A command that a `<(..)` in it interrupts, read on once that closes.
struct Interrupted {
    words: Vec<String>,
    redirected: Vec<String>,
    input: Option<String>,
    from: Option<String>,
    piped: bool,
    output: Option<String>,
}

/// What a `(` or `<(` opened: the directories and variables to go back to
/// when it closes, and the command a `<(` interrupts.
struct Scope {
    dirs: Vec<Option<String>>,
    vars: Variables,
    outer: Option<Interrupted>,
}

/// The file a command reads what a `<(..)` prints from, as the shell
/// names it.
const FED: &str = "/dev/fd/63";

/// Read `command`, `depth` strings deep, seeing the variables `vars` the
/// code around it set.
fn read_in(command: &str, depth: usize, vars: &Variables) -> Reading {
    let tokens = tokens(command);
    if depth > DEPTH {
        let writes = tokens
            .into_iter()
            .filter_map(|token| match token {
                Token::Word(word) => Some(word),
                _ => None,
            })
            .collect();
        return Reading {
            writes,
            ..Reading::default()
        };
    }
    let mut reading = Reading::default();
    let mut vars = vars.clone();
    let mut nested_remote = None;
    // Each simple command's program and the token that ended it.
    let mut commands: Vec<(Option<String>, Option<Token>)> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    // What the command being read names, from where it runs: by
    // redirection, by the commands substituted in it, and by itself.
    let mut found = Reading::default();
    let mut redirected: Vec<String> = Vec::new();
    let mut input: Option<String> = None;
    let mut from: Option<String> = None;
    // What the command before wrote into the pipe, when the line spells it.
    let mut output: Option<String> = None;
    let mut piped = false;
    // Where commands run, relative to where the line starts: a directory
    // for each way the `cd`s before may have gone, `None` for one this
    // reader cannot name. A subshell's `cd` and variables stay inside it.
    let mut dirs: Vec<Option<String>> = vec![Some(String::new())];
    let mut scopes: Vec<Scope> = Vec::new();
    // The files a command wrote text the line spells into, from where the
    // line starts, and that text.
    let mut spelled: Vec<(String, String)> = Vec::new();
    let mut tokens = tokens.into_iter().peekable();
    loop {
        let end = match tokens.next() {
            Some(Token::Word(word)) => {
                words.push(word);
                continue;
            }
            Some(Token::WriteTo) => {
                if let Some(Token::Word(target)) = tokens.next_if(is_word) {
                    redirected.push(target);
                }
                continue;
            }
            Some(Token::ReadFrom) => {
                if let Some(Token::Word(file)) = tokens.next_if(is_word) {
                    from = Some(file);
                }
                continue;
            }
            Some(Token::HereString) => {
                if let Some(Token::Word(text)) = tokens.next_if(is_word) {
                    input = Some(text);
                }
                continue;
            }
            Some(Token::Input(body)) => {
                input = Some(body);
                continue;
            }
            Some(Token::Nested(code)) => {
                merge(&mut found, read_in(&code, depth + 1, &vars));
                continue;
            }
            // The command around a `<(..)` goes on after it, reading it as
            // a file.
            Some(Token::Feeds) => {
                let program = vars.runs(&words).first().and_then(|run| program_of(run));
                commands.push((program, Some(Token::Feeds)));
                scopes.push(Scope {
                    dirs: dirs.clone(),
                    vars: vars.clone(),
                    outer: Some(Interrupted {
                        words: std::mem::take(&mut words),
                        redirected: std::mem::take(&mut redirected),
                        input: input.take(),
                        from: from.take(),
                        piped,
                        output: output.take(),
                    }),
                });
                piped = false;
                continue;
            }
            end => end,
        };
        let mut targets: Vec<String> = Vec::new();
        for target in redirected.drain(..) {
            targets.extend(vars.substituted(&target));
        }
        found.named.extend(targets.iter().cloned());
        found.writes.extend(targets.iter().cloned());
        let text = match input.take() {
            Some(text) => vars.substituted(&text).swap_remove(0),
            None if piped => output.take().unwrap_or_default(),
            None => String::new(),
        };
        let file = from
            .take()
            .map(|file| vars.substituted(&file).swap_remove(0));
        let stdin = match (&file, text.is_empty()) {
            (_, false) => Input::Text(&text),
            (Some(file), true) => Input::File(file),
            (None, true) => Input::Unknown,
        };
        let runs = vars.runs(&words);
        for run in &runs {
            merge(&mut found, changed(run, stdin, depth, &vars));
            found.named.extend(run.iter().cloned());
        }
        // What it writes into the files it is redirected to, when the line
        // spells it, is the text of a script run from one of them after.
        if let Some(text) = runs.first().and_then(|run| output_of(run, stdin)) {
            for target in &targets {
                for dir in &dirs {
                    spelled.push((located(dir.as_deref(), target), text.clone()));
                }
            }
        }
        found.named.extend(file.clone());
        for path in found.writes.drain(..) {
            for dir in &dirs {
                reading.writes.push(located(dir.as_deref(), &path));
            }
        }
        for path in found.named.drain(..) {
            for dir in &dirs {
                reading.named.push(located(dir.as_deref(), &path));
            }
        }
        for script in found.scripts.drain(..) {
            for dir in &dirs {
                let path = located(dir.as_deref(), &script.path);
                let text = script.text.clone().or_else(|| {
                    spelled
                        .iter()
                        .rev()
                        .find(|(written, _)| lexical(written) == lexical(&path))
                        .map(|(_, text)| text.clone())
                });
                reading.scripts.push(Script {
                    path,
                    dir: located(dir.as_deref(), &script.dir),
                    text,
                    ..script.clone()
                });
            }
        }
        for code in found.code.drain(..) {
            for dir in &dirs {
                reading.code.push(Code {
                    dir: located(dir.as_deref(), &code.dir),
                    ..code.clone()
                });
            }
        }
        reading.unread.append(&mut found.unread);
        nested_remote = nested_remote.or(found.remote_code.take());
        let first = runs.first().map(Vec::as_slice).unwrap_or_default();
        let pipes = end == Some(Token::Pipe);
        // A `cd` in a pipeline runs in a subshell of its own.
        if !piped && !pipes {
            moved(&mut dirs, first, end == Some(Token::And));
        }
        output = if pipes {
            let printed: Vec<String> = runs
                .iter()
                .filter_map(|run| output_of(run, stdin))
                .collect();
            (!printed.is_empty()).then(|| printed.join("\n"))
        } else {
            None
        };
        piped = pipes;
        commands.push((program_of(first), end.clone()));
        vars.assign(&words);
        words.clear();
        match end {
            Some(Token::Open) => scopes.push(Scope {
                dirs: dirs.clone(),
                vars: vars.clone(),
                outer: None,
            }),
            Some(Token::Close) => {
                if let Some(scope) = scopes.pop() {
                    dirs = scope.dirs;
                    vars = scope.vars;
                    if let Some(outer) = scope.outer {
                        words = outer.words;
                        words.push(FED.to_string());
                        redirected = outer.redirected;
                        input = outer.input;
                        from = outer.from;
                        piped = outer.piped;
                        output = outer.output;
                    }
                }
            }
            None => break,
            _ => {}
        }
    }

    let remote = commands.windows(2).find_map(|pair| {
        let [(Some(first), end), (Some(second), _)] = pair else {
            return None;
        };
        match end {
            Some(Token::Pipe) if FETCHERS.contains(&first.as_str()) && runs_code(second) => {
                Some(format!("{first} | {second}"))
            }
            Some(Token::Feeds) if runs_code(first) && FETCHERS.contains(&second.as_str()) => {
                Some(format!("{first} <({second})"))
            }
            _ => None,
        }
    });
    reading.remote_code = remote.or(nested_remote);
    let mut seen = HashSet::new();
    reading.writes.retain(|path| {
        !path.is_empty() && path != "/dev/null" && path != "-" && seen.insert(path.clone())
    });
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
    fn redirections_name_the_file_they_write() {
        assert_eq!(writes("echo true > check.sh"), ["check.sh"]);
        assert_eq!(writes("echo x>>check.sh"), ["check.sh"]);
        assert_eq!(
            writes("cargo test 2> err.log >| out.log"),
            ["err.log", "out.log"]
        );
        assert_eq!(writes("make &> 'build log.txt'"), ["build log.txt"]);
        assert_eq!(writes("cargo test 2>&1 | tee -a run.log"), ["run.log"]);
        assert_eq!(writes("cargo test > /dev/null 2>&1"), Vec::<String>::new());
        assert_eq!(writes("rm -f x 2>/dev/null"), ["x"]);
        // Quoted, a `>` is text.
        assert_eq!(
            writes("echo 'a > b' \"c >> d\" e\\>f"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn programs_that_change_their_arguments_name_them() {
        assert_eq!(writes("sed -i 's/x/y/' check.sh"), ["s/x/y/", "check.sh"]);
        assert_eq!(
            writes("sed -i.bak -e s/a/b/ -- -odd.sh"),
            ["s/a/b/", "-odd.sh"]
        );
        assert_eq!(writes("perl -pi -e 's/a/b/' t.sh"), ["s/a/b/", "t.sh"]);
        assert_eq!(writes("rm -rf .flot build"), [".flot", "build"]);
        // One source: the target may be the file written or a directory.
        assert_eq!(
            writes("mv check.sh old.sh"),
            ["check.sh", "old.sh", "old.sh/check.sh"]
        );
        assert_eq!(
            writes("cp /tmp/fake.sh check.sh"),
            ["check.sh", "check.sh/fake.sh"]
        );
        // Into a directory, only what lands in it.
        assert_eq!(
            writes("cp a/x.sh b/y.sh tests/"),
            ["tests/x.sh", "tests/y.sh"]
        );
        assert_eq!(writes("cp x tests/"), ["tests/x"]);
        assert_eq!(writes("cp -t tests a"), ["tests/a"]);
        assert_eq!(writes("cp -r /tmp/fake/tests ."), ["tests"]);
        assert_eq!(writes("mv a tests/"), ["a", "tests/a"]);
        assert_eq!(writes("rsync -a x/ tests/"), ["tests/"]);
        assert_eq!(writes("tar -xf t -C tests"), ["tests"]);
        assert_eq!(
            writes("chmod -x check.sh && touch flot.toml"),
            ["check.sh", "flot.toml"]
        );
        assert_eq!(writes("git checkout HEAD~1 -- check.sh"), ["check.sh"]);
        assert_eq!(writes("git restore --source=main check.sh"), ["check.sh"]);
        assert_eq!(writes("dd if=/dev/zero of=check.sh bs=1"), ["check.sh"]);
        assert_eq!(writes("sudo -E env A=1 /bin/rm x"), ["x"]);
        // An option's value is taken for an operand; it only matters if it
        // names a protected file.
        assert_eq!(writes("FOO=1 truncate -s 0 check.sh"), ["0", "check.sh"]);
    }

    #[test]
    fn reading_and_other_programs_name_nothing() {
        for command in [
            "cat check.sh",
            "sed 's/x/y/' check.sh",
            "grep -i flot flot.toml",
            "cp check.sh",
            "git checkout main",
            "git log -- check.sh",
            "tee < check.sh",
            "python -c \"open('check.sh','w')\"",
            "cargo test --quiet",
            "echo $(rm -rf x",
        ] {
            let reading = read(command);
            let expected: &[&str] = if command.starts_with("echo $(") {
                &["x"]
            } else if command == "git checkout main" {
                // The whole tree changes to `main`.
                &["."]
            } else {
                &[]
            };
            assert_eq!(reading.writes, expected, "{command}");
            assert_eq!(reading.remote_code, None, "{command}");
        }
    }

    #[test]
    fn variables_the_line_sets_are_put_in() {
        assert_eq!(writes("f=check.sh; rm $f"), ["check.sh"]);
        assert_eq!(writes("f=check.sh && rm \"${f}\""), ["check.sh"]);
        assert_eq!(writes("d=tests\nf=$d/a.sh\nrm $f"), ["tests/a.sh"]);
        assert_eq!(writes("f=a; f+=.sh; rm $f"), ["a.sh"]);
        assert_eq!(writes("for f in a b; do rm $f; done"), ["a", "b"]);
        assert_eq!(writes("export f=x; bash -c 'rm $f'"), ["x"]);
        assert_eq!(writes("f='a b'; rm $f"), ["a", "b", "a b"]);
        // A brace group shares the line's variables; a subshell keeps its
        // own.
        assert_eq!(writes("{ f=x; }; rm $f"), ["x"]);
        assert_eq!(writes("(f=x); rm $f"), ["$f"]);
        // An assignment before a program is that program's environment,
        // gone once it ends.
        assert_eq!(writes("f=x rm $f"), ["$f"]);
        assert_eq!(writes("f=x true; rm $f"), ["$f"]);
        // Put in wherever the line spells it, whether the shell that runs
        // it would see it or not.
        assert_eq!(writes("f=x; sh -c 'rm $f'"), ["x"]);
        assert_eq!(writes("f=x; eval rm '$f'"), ["x"]);
        assert_eq!(writes("readonly f=x; rm $f"), ["x"]);
        // Every value a variable may hold, as many as RUNS.
        let many: Vec<String> = (0..100).map(|n| format!("f{n}")).collect();
        let found = writes(&format!("for f in {}; do rm $f; done", many.join(" ")));
        assert_eq!(found.len(), RUNS);
    }

    #[test]
    fn what_a_command_prints_is_built_into_the_word() {
        assert_eq!(
            writes("rm -f $(echo x) check.sh"),
            [format!("{BUILT}"), "check.sh".to_string()]
        );
        assert_eq!(
            writes("rm `echo x` check.sh"),
            [format!("{BUILT}"), "check.sh".to_string()]
        );
        assert_eq!(
            writes("rm \"$(pwd)/check.sh\""),
            [format!("{BUILT}/check.sh")]
        );
        // The substituted command is read as well.
        assert_eq!(writes("echo $(rm x)"), ["x"]);
        assert_eq!(writes("echo $(date)"), Vec::<String>::new());
        // A program named when the line runs may change every word.
        assert_eq!(writes("$(echo rm) check.sh"), ["check.sh"]);
        let unread = |command: &str| {
            read(command)
                .unread
                .into_iter()
                .map(|unread| unread.shape)
                .collect::<Vec<_>>()
        };
        assert_eq!(unread("sh -c \"$(printf 'rm che%s' ck.sh)\""), ["sh $(..)"]);
        assert_eq!(unread("eval \"$(cat x)\""), ["eval $(..)"]);
        assert_eq!(unread("python3 -c \"$(cat x)\""), ["python3 $(..)"]);
        assert_eq!(unread("sh \"$(dirname $0)/x.sh\""), ["sh $(..)"]);
        assert!(unread("echo $(date)").is_empty());
        assert!(unread("node \"$(dirname $0)/x.js\"").is_empty());
        // `<(..)` is a file the command around it reads.
        assert_eq!(writes("diff <(sort a) b.sh && rm c"), ["c"]);
        assert!(read("diff <(sort a) ./tool.mjs").scripts.is_empty());
        // `command -v` runs nothing.
        assert_eq!(writes("command -v unzip"), Vec::<String>::new());
    }

    #[test]
    fn code_given_to_an_interpreter_is_handed_back() {
        let code = |command: &str| {
            read(command)
                .code
                .into_iter()
                .map(|code| (code.program, code.text))
                .collect::<Vec<_>>()
        };
        let one = |program: &str, text: &str| vec![(program.to_string(), text.to_string())];
        assert_eq!(code("python3 -c 'print(1)'"), one("python3", "print(1)"));
        assert_eq!(code("python3.12 -Bc 'x'"), one("python3.12", "x"));
        assert_eq!(code("node -e 'a' -p 'b'"), one("node", "a\nb"));
        assert_eq!(code("node --eval='a'"), one("node", "a"));
        assert_eq!(code("perl -ne 'print' f"), one("perl", "print"));
        assert_eq!(code("perl -i.bak -pe 's/a/b/' f"), one("perl", "s/a/b/"));
        assert_eq!(code("ruby -e 'x'"), one("ruby", "x"));
        assert_eq!(code("php -r 'x;'"), one("php", "x;"));
        assert_eq!(code("deno eval 'x'"), one("deno", "x"));
        assert_eq!(code("Rscript -e 'x'"), one("Rscript", "x"));
        assert_eq!(code("lua -e 'x'"), one("lua", "x"));
        assert_eq!(code("osascript -e 'x'"), one("osascript", "x"));
        assert_eq!(code("python3 - <<'EOF'\nx\nEOF"), one("python3", "x"));
        assert_eq!(code("python3 <<< 'x'"), one("python3", "x"));
        assert_eq!(code("echo 'x' | python3"), one("python3", "x"));
        assert_eq!(code("cd sub && python3 -c x")[0].1, "x");
        assert_eq!(read("cd sub && python3 -c x").code[0].dir, "sub");
        assert!(code("python3 -m pytest -c x").is_empty());
        assert!(code("python3 script.py -c x").is_empty());
    }

    #[test]
    fn scripts_a_command_runs_are_handed_back() {
        let scripts = |command: &str| {
            read(command)
                .scripts
                .into_iter()
                .map(|script| (script.path, script.runner, script.depth))
                .collect::<Vec<_>>()
        };
        let shell = |path: &str| vec![(path.to_string(), Runner::Shell, 0)];
        assert_eq!(scripts("sh x.sh"), shell("x.sh"));
        assert_eq!(scripts("bash -e -o pipefail x.sh a"), shell("x.sh"));
        assert_eq!(scripts(". x.sh"), shell("x.sh"));
        assert_eq!(scripts("source x.sh"), shell("x.sh"));
        assert_eq!(scripts("sh < x.sh"), shell("x.sh"));
        assert_eq!(scripts("cd d && sh x.sh"), shell("d/x.sh"));
        assert_eq!(
            scripts("./x"),
            vec![("./x".to_string(), Runner::Program, 0)]
        );
        assert_eq!(
            scripts("python3 x.py"),
            vec![(
                "x.py".to_string(),
                Runner::Interpreter("python3".to_string()),
                0
            )]
        );
        assert_eq!(
            scripts("sh -c 'sh x.sh'"),
            vec![("x.sh".to_string(), Runner::Shell, 1)]
        );
        assert!(scripts("sh -c 'echo'").is_empty());
        let written = read("cat > x.sh <<'EOF'\nrm a\nEOF\ncd d && sh ../x.sh").scripts;
        assert_eq!(written[0].text.as_deref(), Some("rm a"));
        assert_eq!(
            read("echo 'rm a' > x.sh; sh x.sh").scripts[0]
                .text
                .as_deref(),
            Some("rm a")
        );
        assert_eq!(read("sh x.sh; echo 'rm a' > x.sh").scripts[0].text, None);
        assert_eq!(
            read("printf 'X=%s\\nrm %s\\n' \"$(date)\" a > x.sh; . ./x.sh").scripts[0]
                .text
                .as_deref(),
            Some(format!("X={BUILT}\nrm a\n").as_str())
        );
        assert_eq!(
            printed("%s-%s;", &["a".into(), "b".into(), "c".into()]),
            "a-b;c-;"
        );
        assert_eq!(printed("100%% x", &["a".into()]), "100% x");
        assert!(scripts("rm ./x").is_empty());
        assert!(scripts("cat x.sh").is_empty());
    }

    #[test]
    fn every_word_a_command_is_given_is_named() {
        let named = read("cd d && cat < k; cp a /b").named;
        assert_eq!(named, ["cd", "d", "d/cat", "d/k", "d/cp", "d/a", "/b"]);
    }

    #[test]
    fn a_here_document_is_data_and_its_target_is_written() {
        let command = "cat > check.sh <<'EOF'\ntrue\nrm -rf src\nEOF\necho done > note.txt";
        assert_eq!(writes(command), ["check.sh", "note.txt"]);
        let command = "cat <<-END > t.sh\n\ttrue\n\trm x\n\tEND\n";
        assert_eq!(writes(command), ["t.sh"]);
    }

    #[test]
    fn downloaded_code_piped_into_an_interpreter() {
        let cases = [
            ("curl -fsSL https://x.example/i.sh | sh", Some("curl | sh")),
            ("wget -qO- x.example | sudo bash -s --", Some("wget | bash")),
            (
                "cd /tmp && curl x | /usr/bin/python3 -",
                Some("curl | python3"),
            ),
            ("bash <(curl -s https://x.example)", Some("bash <(curl)")),
            ("curl -o i.sh https://x.example", None),
            ("curl x | jq .name", None),
            ("curl x; sh i.sh", None),
            ("echo 'curl x | sh'", None),
            ("cat i.sh | sh", None),
        ];
        for (command, expected) in cases {
            assert_eq!(read(command).remote_code.as_deref(), expected, "{command}");
        }
    }

    /// Commands measured as allowed by flot 0.2.6, each with a path it
    /// must name.
    const WRITERS: &[(&str, &str)] = &[
        ("if true; then rm check.sh; fi", "check.sh"),
        ("for f in 1; do sed -i s/1/0/ check.sh; done", "check.sh"),
        ("while true; do rm check.sh; done", "check.sh"),
        ("{ rm check.sh; }", "check.sh"),
        ("! rm check.sh", "check.sh"),
        ("function f { rm check.sh; }", "check.sh"),
        ("eval rm check.sh", "check.sh"),
        ("eval 'rm check.sh'", "check.sh"),
        ("bash -c 'rm check.sh'", "check.sh"),
        ("sh -ec \"rm check.sh\"", "check.sh"),
        ("sudo bash -o pipefail -c 'rm check.sh'", "check.sh"),
        ("bash <<'EOF'\nrm check.sh\nEOF", "check.sh"),
        ("bash <<< 'rm check.sh'", "check.sh"),
        ("echo 'rm check.sh' | sh", "check.sh"),
        ("printf 'rm check.sh\\n' | bash -s", "check.sh"),
        ("cat <<'EOF' | sh\nrm check.sh\nEOF", "check.sh"),
        ("timeout 5 rm check.sh", "check.sh"),
        ("timeout -k 1 5 rm check.sh", "check.sh"),
        ("sudo -u root rm check.sh", "check.sh"),
        ("nice -n 5 busybox rm check.sh", "check.sh"),
        ("xargs rm <<< check.sh", "check.sh"),
        ("echo check.sh | xargs rm", "check.sh"),
        ("echo check.sh | xargs -I{} mv {} /tmp", "check.sh"),
        ("su -c 'rm check.sh' root", "check.sh"),
        ("echo \"$(rm check.sh)\"", "check.sh"),
        ("echo \"`rm check.sh`\"", "check.sh"),
        ("cat <<EOF\n$(rm check.sh)\nEOF", "check.sh"),
        ("echo x >& check.sh", "check.sh"),
        ("echo x > $'check.sh'", "check.sh"),
        ("echo x > $'check\\x2esh'", "check.sh"),
        ("exec 3<> check.sh", "check.sh"),
        (
            "# it doesn't pass yet\nsed -i 's/1/0/' check.sh",
            "check.sh",
        ),
        ("x=$((1<<4))\nrm check.sh", "check.sh"),
        ("(( x = 1 << 4 ))\nrm check.sh", "check.sh"),
        ("echo $(( $(rm check.sh) + 1 ))", "check.sh"),
        ("cat <<'E F'\nbody\nE F\nrm check.sh", "check.sh"),
        ("cd tests && rm accept.sh", "tests/accept.sh"),
        ("cd tests; rm accept.sh", "tests/accept.sh"),
        // `cd` may fail: the command after `;` may run here.
        ("cd tests; rm check.sh", "check.sh"),
        (
            "cd a && cd ../tests && rm accept.sh",
            "a/../tests/accept.sh",
        ),
        ("(cd tests && ls); rm check.sh", "check.sh"),
        ("cd tests && rm \"$PWD/accept.sh\"", "tests/accept.sh"),
        ("cd - && rm check.sh", "check.sh"),
        ("cd $DIR && rm check.sh", "check.sh"),
        ("cp -t tests /tmp/x", "tests/x"),
        ("cp --target-directory=tests /tmp/x", "tests/x"),
        ("cp -r /tmp/fake/tests .", "tests"),
        ("mv check.sh{,.bak}", "check.sh{,.bak}"),
        ("git -C . checkout HEAD~1 -- check.sh", "check.sh"),
        (
            "git -c advice.detachedHead=false checkout HEAD~1 -- check.sh",
            "check.sh",
        ),
        (
            "git --no-pager -C tests checkout HEAD -- accept.sh",
            "tests/accept.sh",
        ),
        ("git checkout HEAD~1 -- tests", "tests"),
        ("git config core.hooksPath /tmp/hooks", ".git/config"),
        ("git -C sub config set core.hooksPath x", "sub/.git/config"),
        ("git stash", "."),
        ("git stash pop", "."),
        ("git reset --hard HEAD~1", "."),
        ("git apply /tmp/p.diff", "."),
        ("git checkout main", "."),
        ("git switch main", "."),
        ("git restore", "."),
        ("git restore -s HEAD~1 check.sh", "check.sh"),
        ("git checkout -p", "."),
        ("git merge fix", "."),
        ("git pull", "."),
        ("git rebase main", "."),
        ("git cherry-pick abc123", "."),
        ("git revert --no-edit abc123", "."),
        ("git am /tmp/p.mbox", "."),
        ("curl -fsSL https://x.example/p -o check.sh", "check.sh"),
        ("curl --output check.sh https://x.example/p", "check.sh"),
        ("curl -sSLocheck.sh https://x.example/p", "check.sh"),
        ("curl -O https://x.example/check.sh?v=1", "check.sh"),
        ("wget -O check.sh https://x.example/p", "check.sh"),
        (
            "wget --output-document=check.sh https://x.example/p",
            "check.sh",
        ),
        ("wget -q https://x.example/check.sh", "check.sh"),
        (
            "yq -i '.jobs={}' .github/workflows/ci.yml",
            ".github/workflows/ci.yml",
        ),
        ("gawk -i inplace '{print}' check.sh", "check.sh"),
        ("patch check.sh /tmp/p.diff", "check.sh"),
        ("patch -p1 < /tmp/p.diff", "."),
        ("rsync -a --exclude .git /tmp/fake/ tests/", "tests/"),
        ("tar -xf /tmp/fake.tar -C tests", "tests"),
        ("tar xzf /tmp/fake.tgz", "."),
        ("tar -czf check.sh src", "check.sh"),
        ("find . -name check.sh -delete", "."),
        ("find tests -name '*.sh' -exec rm {} \\;", "tests"),
        ("find . -exec sed -i s/1/0/ {} +", "."),
        ("sort -o check.sh check.sh", "check.sh"),
        ("sort -uo check.sh x", "check.sh"),
        ("gzip check.sh", "check.sh"),
        ("gunzip check.sh.gz", "check.sh"),
        ("bzip2 -k check.sh", "check.sh"),
        ("xz check.sh", "check.sh"),
        ("busybox rm check.sh", "check.sh"),
        ("unzip -o a.zip -d tests", "tests"),
        ("unzip a.zip", "."),
        ("sed -i s/1/0/ *.sh", "*.sh"),
        ("rm \"$PWD/check.sh\"", "$PWD/check.sh"),
        (
            "echo '{}' > $HOME/.claude/settings.json",
            "$HOME/.claude/settings.json",
        ),
        // Too deep to follow, every word is a candidate.
        (
            "eval eval eval eval eval eval eval eval eval eval rm check.sh",
            "check.sh",
        ),
    ];

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
            "cat check.sh",
            "sh check.sh",
            "bash -x check.sh",
            "grep -n 1 check.sh",
            "git commit -m 'fix check.sh'",
            "git diff -- check.sh",
            "git status",
            "git log -- check.sh",
            "git show HEAD:check.sh",
            "git checkout -b fix",
            "git switch -c fix",
            "git config core.hooksPath",
            "git stash list",
            "git reset -- check.sh",
            "echo check.sh",
            "ls tests",
            "cargo test",
            "diff a check.sh",
            "cp check.sh /tmp/x",
            "tar -tf a.tar",
            "tar -xOf a.tar check.sh",
            "unzip -l a.zip",
            "gzip -c check.sh",
            "sort check.sh",
            "find . -name check.sh",
            "find . -name check.sh -exec grep -l x {} \\;",
            "curl -fsSL https://x.example/check.sh",
            "wget -qO- https://x.example/check.sh",
            "rsync -n -a check.sh tests/",
            "awk '{print}' check.sh",
            "patch --dry-run check.sh p.diff",
            "# rm check.sh",
            "echo '$(rm check.sh)'",
            "cat <<'EOF'\n$(rm check.sh)\nEOF",
            "cd tests && cat accept.sh",
            "bash -c 'cat check.sh'",
            "eval cat check.sh",
            "echo check.sh | xargs cat",
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

    #[test]
    fn stopping_or_resuming_a_git_operation_writes_nothing_new() {
        for command in [
            "git merge --abort",
            "git rebase --continue",
            "git cherry-pick --abort",
            "git revert --quit",
            "git am --skip",
            "git checkout -b fix",
            "git stash list",
            "wget -q --spider https://x.example/check.sh",
        ] {
            assert_eq!(writes(command), Vec::<String>::new(), "{command}");
        }
    }
}
