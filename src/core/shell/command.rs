//! A simple command's program, past assignments and wrappers, and its
//! options and operands.

use super::tables::{KEYWORDS, WRAPPERS};

pub(super) fn program(word: &str) -> &str {
    word.rsplit(['/', '\\']).next().unwrap_or(word)
}

/// A simple command from its program on.
pub(super) struct Command<'a> {
    pub(super) words: &'a [String],
    /// Run by `xargs`: with the string it replaces by each item, if any.
    pub(super) xargs: Option<Option<String>>,
    /// Whether a wrapper or a reserved word comes before it.
    pub(super) wrapped: bool,
    /// Whether a wrapper runs a shell when it is given no command, as
    /// `sudo -s` and `sudo -i` do.
    pub(super) shell: bool,
}

/// A simple command past reserved words, variable assignments and
/// wrappers such as `sudo -E env A=1`, with their options.
pub(super) fn command(words: &[String]) -> Command<'_> {
    let mut at = 0;
    let mut wrapped = false;
    let mut shell = false;
    while let Some(word) = words.get(at) {
        if word == "function" {
            // `function f { rm x; }` runs from its body.
            match words[at..].iter().position(|word| word == "{") {
                Some(open) => at += open + 1,
                None => at = words.len(),
            }
        } else if KEYWORDS.contains(&word.as_str()) {
            at += 1;
            wrapped = true;
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
        // A system that does not tell case apart runs `SUDO` as `sudo`.
        let Some((name, short, long, mut operands)) = WRAPPERS
            .iter()
            .copied()
            .find(|(wrapper, ..)| wrapper.eq_ignore_ascii_case(program(word)))
        else {
            break;
        };
        at += 1;
        wrapped = true;
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
                        wrapped,
                        shell: false,
                    };
                }
                shell |= matches!(name, "sudo" | "doas") && runs_shell(arg, short);
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
        wrapped,
        shell,
    }
}

/// Whether `arg`, an option of `sudo` or `doas` whose letters in `short`
/// take a value, asks for a shell: `-s`, `-i`, `--shell` or `--login`.
fn runs_shell(arg: &str, short: &str) -> bool {
    match arg.strip_prefix("--") {
        Some(long) => matches!(long, "shell" | "login"),
        None => arg
            .chars()
            .skip(1)
            .take_while(|letter| !short.contains(*letter))
            .any(|letter| matches!(letter, 's' | 'i')),
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
pub(super) struct Parsed<'a> {
    options: Vec<(&'a str, Option<&'a str>)>,
    pub(super) operands: Vec<&'a str>,
    /// Where in `operands` those after `--` start.
    pub(super) dashes: Option<usize>,
}

impl<'a> Parsed<'a> {
    pub(super) fn has(&self, names: &[&str]) -> bool {
        self.options.iter().any(|(name, _)| names.contains(name))
    }

    pub(super) fn values(&self, names: &[&str]) -> Vec<&'a str> {
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
pub(super) fn parse<'a>(args: &'a [String], short: &str, long: &[&str]) -> Parsed<'a> {
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

pub(super) fn strings(values: Vec<&str>) -> Vec<String> {
    values.into_iter().map(str::to_string).collect()
}
