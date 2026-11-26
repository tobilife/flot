//! What a command substituted in a word prints, as far as this reader
//! works it out.

use crate::core::reading::{BUILT, DEPTH, MKTEMP, SPELLED};

use super::command::{command, program};
use super::lex::{is_word, tokens, Token, Word};
use super::paths::{joined, lexical, program_of};
use super::tables::runs_code;
use super::vars::Variables;
use super::Origin;

/// Programs whose output is text the command line spells or decodes, or
/// the contents of a file the agent may have written, or of a download.
/// A shell or an interpreter running code prints what its code spells too.
const SPELLERS: &[&str] = &[
    "printf", "echo", "base64", "base32", "basenc", "xxd", "rev", "tr", "sed", "awk", "gawk",
    "mawk", "nawk", "perl", "python", "python3", "ruby", "cut", "paste", "iconv", "openssl", "cat",
    "curl", "wget", "eval",
];

/// Programs that print a path they work out of what they are given.
const PATHS: &[&str] = &["dirname", "basename", "pwd", "realpath", "readlink", "cd"];

/// Whether what the program `name` prints is text a command line can
/// spell.
fn spells(name: &str) -> bool {
    SPELLERS.contains(&name) || runs_code(name)
}

/// `word` with what each command substituted in it prints put in, as
/// [`printed_by`] works it out, `level` substitutions deep.
pub(super) fn resolved(word: Word, level: usize, vars: &Variables, origin: &Origin) -> String {
    if word.printed.is_empty() {
        return word.text;
    }
    let mut text = String::with_capacity(word.text.len() + 16);
    let mut at = 0;
    for (offset, inner) in &word.printed {
        text.push_str(&word.text[at..*offset]);
        text.push_str(&printed_by(inner, level, vars, origin));
        at = *offset;
    }
    text.push_str(&word.text[at..]);
    text
}

/// What the command `inner`, substituted in a word `level` substitutions
/// deep, prints, with the variables `vars`, run where `origin` says: the
/// name `mktemp` makes; the
/// path a program that only works one out prints, when what it is given is
/// known (`dirname`, `basename`, `pwd`, `realpath`, `readlink -f`, `cd X &&
/// pwd`); [`SPELLED`] when a program anywhere in it makes text (`$(< f)`
/// is `cat`), or works a path out of text one made; [`BUILT`] for anything
/// else.
fn printed_by(inner: &str, level: usize, vars: &Variables, origin: &Origin) -> String {
    if level >= DEPTH {
        return SPELLED.to_string();
    }
    let program = inner.split_whitespace().next().unwrap_or_default();
    let simple = !inner.contains([';', '&', '|', '<', '>', '$', '`', '(', '\n']);
    if simple && matches!(program, "mktemp" | "/usr/bin/mktemp" | "/bin/mktemp") {
        return MKTEMP.to_string();
    }
    let mut spelled = false;
    let mut commands: Vec<(Vec<String>, Option<Token>)> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut input = false;
    let mut tokens = tokens(inner, origin).into_iter().peekable();
    loop {
        let end = match tokens.next() {
            Some(Token::Word(word)) => {
                words.push(resolved(word, level + 1, vars, origin));
                continue;
            }
            Some(token @ (Token::WriteTo | Token::ReadFrom | Token::HereString)) => {
                input |= token == Token::ReadFrom;
                tokens.next_if(is_word);
                continue;
            }
            Some(Token::Input(_) | Token::Nested(_)) => continue,
            end => end,
        };
        spelled |= input && words.is_empty();
        input = false;
        let done = end.is_none();
        commands.push((std::mem::take(&mut words), end));
        if done {
            break;
        }
    }
    commands.retain(|(words, _)| !words.is_empty());
    let mut runs: Vec<Vec<Vec<String>>> = Vec::new();
    for (words, _) in &commands {
        let these = vars.runs(words);
        spelled |= these.iter().any(|run| {
            program_of(run).is_some_and(|name| {
                spells(&name)
                    || name.contains(SPELLED)
                    || (PATHS.contains(&name.as_str())
                        && run.iter().any(|word| word.contains(SPELLED)))
            })
        });
        runs.push(these);
    }
    if spelled {
        return SPELLED.to_string();
    }
    let here = origin.dir.as_deref();
    let value = match (commands.as_slice(), runs.as_slice()) {
        ([_], [run]) if run.len() == 1 => produced(&run[0], here),
        ([(_, Some(Token::And)), _], [cd, pwd]) if cd.len() == 1 && pwd.len() == 1 => {
            entered(&cd[0], &pwd[0], here)
        }
        _ => None,
    };
    value.unwrap_or_else(|| BUILT.to_string())
}

/// The operands of a command's `args` past `--` and the options in
/// `options`, when each is known text: no variable, wildcard or output of
/// another command in it. `None` for another option or an unknown operand.
fn known_operands<'a>(args: &'a [String], options: &[&str]) -> Option<Vec<&'a str>> {
    let mut operands = Vec::new();
    let mut ended = false;
    for arg in args {
        if !ended && arg == "--" {
            ended = true;
        } else if !ended && arg.len() > 1 && arg.starts_with('-') {
            if !options.contains(&arg.as_str()) {
                return None;
            }
        } else if arg.is_empty() || arg.contains(['$', '`', '*', '?', '[', '{', BUILT, SPELLED]) {
            return None;
        } else {
            operands.push(arg.as_str());
        }
    }
    Some(operands)
}

/// `path` as an absolute path, from `here` when it is relative: `None`
/// when that is not known.
fn absolute(path: &str, here: Option<&str>) -> Option<String> {
    let path = if path.starts_with('/') || path == "~" || path.starts_with("~/") {
        path.to_string()
    } else {
        joined(here?, path)
    };
    let path = lexical(&path);
    let trimmed = path.trim_end_matches('/');
    Some(if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    })
}

/// What a simple command of `words` prints when it only works out a path:
/// `dirname`, `basename`, `pwd`, `realpath` and `readlink -f`, given known
/// operands.
fn produced(words: &[String], here: Option<&str>) -> Option<String> {
    let (name, args) = command(words).words.split_first()?;
    match program(name) {
        "dirname" => match known_operands(args, &[])?.as_slice() {
            [path] => Some(dirname(path)),
            _ => None,
        },
        "basename" => match known_operands(args, &[])?.as_slice() {
            [path] => Some(basename(path, "")),
            [path, suffix] => Some(basename(path, suffix)),
            _ => None,
        },
        "pwd" => {
            if !known_operands(args, &["-L", "-P"])?.is_empty() {
                return None;
            }
            absolute(".", here)
        }
        "realpath" => match known_operands(args, &["-e", "-m", "-s", "-q", "-L", "-P"])?.as_slice()
        {
            [path] => absolute(path, here),
            _ => None,
        },
        "readlink"
            if args
                .iter()
                .any(|arg| matches!(arg.as_str(), "-f" | "-e" | "-m")) =>
        {
            match known_operands(args, &["-f", "-e", "-m", "-n", "-q", "-s"])?.as_slice() {
                [path] => absolute(path, here),
                _ => None,
            }
        }
        _ => None,
    }
}

/// What `cd DIR && pwd` prints: `DIR`, absolute.
fn entered(cd: &[String], pwd: &[String], here: Option<&str>) -> Option<String> {
    let (name, args) = command(cd).words.split_first()?;
    let (print, options) = command(pwd).words.split_first()?;
    if program(name) != "cd" || program(print) != "pwd" {
        return None;
    }
    if !known_operands(options, &["-L", "-P"])?.is_empty() {
        return None;
    }
    match known_operands(args, &["-L", "-P", "-e"])?.as_slice() {
        [dir] if *dir != "-" => absolute(dir, here),
        _ => None,
    }
}

/// What `dirname path` prints.
pub(super) fn dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() { "." } else { "/" }.to_string();
    }
    match trimmed.rfind('/') {
        None => ".".to_string(),
        Some(at) => match trimmed[..at].trim_end_matches('/') {
            "" => "/".to_string(),
            dir => dir.to_string(),
        },
    }
}

/// What `basename path suffix` prints.
pub(super) fn basename(path: &str, suffix: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() { "" } else { "/" }.to_string();
    }
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    match name.strip_suffix(suffix) {
        Some(stem) if !suffix.is_empty() && !stem.is_empty() => stem.to_string(),
        _ => name.to_string(),
    }
}
