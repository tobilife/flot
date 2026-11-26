//! The reader loop: a command line, token by token.

use std::collections::HashSet;

use crate::core::reading::{Reading, Script, DEPTH};
use crate::core::shell;

use super::cmdlets::{command_name, Argument, Piped, EVALUATORS, FETCHERS};
use super::lex::{tokens, Token};
use super::paths::{dotnet, file_path, located, reads_only, Method};
use super::run::changed_at;
use super::vars::{moved, piped, Variables, FILTERS, FOR_EACH};

/// A script block or a nested expression being read.
struct Block {
    /// What `$_` stood for outside it.
    item: Option<Vec<String>>,
    /// What a `Where-Object` block passes on when it closes.
    passes: Option<Piped>,
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
