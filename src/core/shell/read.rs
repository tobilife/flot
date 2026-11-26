//! The reader loop: a command line, token by token.

use std::collections::HashSet;

use crate::core::reading::{merge, overflowed, told, Code, Command, Reading, Script, DEPTH};

use super::lex::{is_word, tokens, Token};
use super::paths::{joined, lexical, located, moved, output_of, program_of};
use super::run::{changed, Input};
use super::substitution::resolved;
use super::tables::{runs_code, FETCHERS, KEYWORDS};
use super::vars::Variables;
use super::Origin;

/// Read `command`, code handed over as a string `depth` times, started as
/// `origin` says. A script's text is read one deeper than the code that
/// runs it, so past [`DEPTH`], where every word is taken for a path it may
/// write, counts both.
pub fn read_at(command: &str, depth: usize, origin: &Origin) -> Reading {
    read_in(command, depth, &Variables::default(), origin)
}

/// The absolute directory commands run in, from `here`, where the line
/// starts, past the `cd`s that led to `dirs`: `None` unless that is one
/// directory this reader can name.
fn here_at(here: Option<&str>, dirs: &[Option<String>]) -> Option<String> {
    match dirs {
        [Some(dir)] if dir.starts_with('/') => Some(lexical(dir)),
        [Some(dir)] if !dir.starts_with('~') => Some(lexical(&joined(here?, dir))),
        _ => None,
    }
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
/// code around it set, where `origin` says.
pub(super) fn read_in(command: &str, depth: usize, vars: &Variables, origin: &Origin) -> Reading {
    let tokens = tokens(command, origin);
    if depth > DEPTH {
        // The words of each simple command, split at every operator.
        let mut simple: Vec<Vec<String>> = vec![Vec::new()];
        for token in tokens {
            match token {
                Token::Word(word) => simple
                    .last_mut()
                    .expect("one at least")
                    .push(resolved(word, DEPTH, vars, origin)),
                _ => simple.push(Vec::new()),
            }
        }
        let mut reading = Reading {
            writes: simple.concat(),
            ..Reading::default()
        };
        overflowed(&mut reading, &simple);
        return reading;
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
    // Where the next command runs, past the `cd`s before it.
    let mut place = origin.clone();
    let mut scopes: Vec<Scope> = Vec::new();
    // The files a command wrote text the line spells into, from where the
    // line starts, and that text.
    let mut spelled: Vec<(String, String)> = Vec::new();
    // How many `case`s are open, and whether their patterns are being
    // read: a pattern is no command, as `patch)` in `minor | patch)`.
    let mut cases = 0usize;
    let mut patterns = false;
    let mut tokens = tokens.into_iter().peekable();
    loop {
        let token = tokens.next();
        if patterns {
            match token {
                Some(Token::Word(word)) if word.text == "esac" && word.printed.is_empty() => {
                    cases -= 1;
                    patterns = false;
                    continue;
                }
                Some(Token::Close) => {
                    patterns = false;
                    continue;
                }
                Some(Token::Nested(code)) => {
                    merge(&mut found, read_in(&code, depth + 1, &vars, &place));
                    continue;
                }
                Some(_) => continue,
                None => {}
            }
        }
        let end = match token {
            Some(Token::Word(word)) => {
                if word.printed.is_empty() {
                    let start = words
                        .iter()
                        .position(|word| !KEYWORDS.contains(&word.as_str()))
                        .unwrap_or(words.len());
                    if cases > 0 && word.text == "esac" && words.len() == start {
                        cases -= 1;
                        continue;
                    }
                    if word.text == "in" && words.len() == start + 2 && words[start] == "case" {
                        words.clear();
                        cases += 1;
                        patterns = true;
                        continue;
                    }
                }
                words.push(resolved(word, 0, &vars, &place));
                continue;
            }
            Some(token @ (Token::WriteTo | Token::ReadFrom | Token::HereString)) => {
                if let Some(Token::Word(word)) = tokens.next_if(is_word) {
                    let word = resolved(word, 0, &vars, &place);
                    match token {
                        Token::WriteTo => redirected.push(word),
                        Token::ReadFrom => from = Some(word),
                        _ => input = Some(word),
                    }
                }
                continue;
            }
            Some(Token::Input(body)) => {
                input = Some(body);
                continue;
            }
            Some(Token::Nested(code)) => {
                merge(&mut found, read_in(&code, depth + 1, &vars, &place));
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
            merge(&mut found, changed(run, stdin, depth, &vars, &place));
            found.named.extend(run.iter().cloned());
        }
        // A variable may hold what this reader did not see it set to, by
        // `read`, `eval` or a script it sources: the commands as written,
        // with no variable put in, are told too.
        if runs.iter().any(|run| *run != words) {
            let written = changed(&words, stdin, depth, &Variables::default(), &place);
            found.commands.extend(told(written));
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
        for command in found.commands.drain(..) {
            for dir in &dirs {
                reading.commands.push(Command {
                    dir: located(dir.as_deref(), &command.dir),
                    writes: command
                        .writes
                        .iter()
                        .map(|path| located(dir.as_deref(), path))
                        .collect(),
                    ..command.clone()
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
        patterns = end == Some(Token::CaseEnd) && cases > 0;
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
        place.dir = here_at(origin.dir.as_deref(), &dirs);
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
