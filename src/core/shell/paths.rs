//! Paths as the line names them, and the small helpers words need.

use std::collections::HashSet;

use crate::core::reading::{BUILT, SPELLED};

use super::command::{command, program};
use super::run::Input;

/// The last name in `path`.
pub(super) fn base(path: &str) -> &str {
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
pub(super) fn lexical(path: &str) -> String {
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
    if path.starts_with(['/', '~', '$', BUILT, SPELLED]) {
        path.to_string()
    } else {
        joined(dir, path)
    }
}

/// Moves `dirs` past `words` when it is a `cd`: to the directory it names,
/// or to one this reader cannot name (`cd -`, `cd $X`). Only after
/// `cd x &&` do the commands surely run there; otherwise they may run in
/// either place. `cd` and `cd $HOME` lead to `~`, or wherever `HOME` is
/// set to when the line runs, which this reader cannot tell: they may run
/// in the place a `cd` it cannot follow leads to as well.
pub(super) fn moved(dirs: &mut Vec<Option<String>>, words: &[String], certain: bool) {
    let Some((name, args)) = command(words).words.split_first() else {
        return;
    };
    let operand = args
        .iter()
        .map(String::as_str)
        .find(|arg| *arg == "-" || !arg.starts_with('-'));
    let home = |operand: &str| {
        ["$HOME", "${HOME}"].iter().find_map(|home| {
            operand
                .strip_prefix(home)
                .filter(|rest| rest.is_empty() || rest.starts_with('/'))
                .map(|rest| format!("~{rest}"))
        })
    };
    let (target, certain) = match (program(name), operand) {
        ("cd", None) => (Some("~".to_string()), false),
        ("cd" | "pushd", Some(operand)) => match home(operand) {
            Some(home) => (Some(home), false),
            None => (Some(operand.to_string()), certain),
        },
        ("pushd" | "popd", _) => (None, certain),
        _ => return,
    };
    let target = target.filter(|dir| dir != "-" && !dir.contains(['$', '`', BUILT, SPELLED]));
    let next: Vec<Option<String>> = dirs
        .iter()
        .map(|dir| {
            let target = target.as_deref()?;
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
pub(super) fn output_of(words: &[String], input: Input) -> Option<String> {
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
pub(super) fn printed(format: &str, values: &[String]) -> String {
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
pub(super) fn program_of(words: &[String]) -> Option<String> {
    command(words)
        .words
        .first()
        .map(|word| program(word).to_string())
}

/// Whether `name` can name a shell variable.
pub(super) fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The name and value of `NAME=value` or `NAME+=value`, the name with its
/// `+`.
pub(super) fn assignment(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    is_name(name.strip_suffix('+').unwrap_or(name)).then_some((name, value))
}
