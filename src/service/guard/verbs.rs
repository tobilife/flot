//! The `flot` commands a command line runs that an agent should leave to
//! the user, found by a reader of its own that splits the line into simple
//! commands.

use super::Shell;

/// One simple command of a command line: its words, quotes removed, and
/// where it is in the line.
pub(super) struct Simple {
    pub(super) words: Vec<String>,
    pub(super) span: std::ops::Range<usize>,
}

/// The simple commands of `command`, split on unquoted `;`, `&`, `|`,
/// newlines, parentheses, braces and, in a POSIX shell, backquotes. Close
/// enough to find a program and what follows it, and no more: what is
/// found here is only ever a reason to refuse.
pub(super) fn simple_commands(command: &str, language: Shell) -> Vec<Simple> {
    let escape = match language {
        Shell::Posix => '\\',
        Shell::PowerShell => '`',
    };
    let mut simples = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut start = 0;
    let mut chars = command.char_indices().peekable();
    let end_word = |word: &mut String, quoted: &mut bool, words: &mut Vec<String>| {
        if !word.is_empty() || *quoted {
            words.push(std::mem::take(word));
        }
        *quoted = false;
    };
    // The character before, outside quotes: a `#` after a blank or an
    // operator starts a comment.
    let mut before: Option<char> = None;
    while let Some((at, c)) = chars.next() {
        let separates = matches!(c, ';' | '&' | '|' | '\n' | '(' | ')' | '{' | '}')
            || (c == '`' && matches!(language, Shell::Posix));
        let comment = c == '#'
            && word.is_empty()
            && !quoted
            && before.is_none_or(|before| before.is_whitespace() || ";&|()".contains(before));
        before = Some(c);
        if c == '\'' {
            quoted = true;
            for (_, c) in chars.by_ref() {
                if c == '\'' {
                    break;
                }
                word.push(c);
            }
        } else if c == '"' {
            quoted = true;
            while let Some((_, c)) = chars.next() {
                if c == '"' {
                    break;
                }
                if c == escape {
                    word.extend(chars.next().map(|(_, next)| next));
                } else {
                    word.push(c);
                }
            }
        } else if c == escape {
            word.extend(
                chars
                    .next()
                    .map(|(_, next)| next)
                    .filter(|next| *next != '\n'),
            );
        } else if comment {
            // A comment runs to the end of the line: `git reset --hard #
            // why` gives git no `#`.
            while chars.next_if(|(_, next)| *next != '\n').is_some() {}
        } else if c == '$' && chars.peek().is_some_and(|(_, next)| *next == '{') {
            // `${..}` is one word, not a brace group: `${#x}` is no comment.
            word.push(c);
            let mut depth = 0;
            for (_, c) in chars.by_ref() {
                word.push(c);
                match c {
                    '{' => depth += 1,
                    '}' if depth == 1 => break,
                    '}' => depth -= 1,
                    _ => {}
                }
            }
        } else if separates {
            end_word(&mut word, &mut quoted, &mut words);
            if !words.is_empty() {
                simples.push(Simple {
                    words: std::mem::take(&mut words),
                    span: start..at,
                });
            }
            start = at + c.len_utf8();
        } else if c.is_whitespace() {
            end_word(&mut word, &mut quoted, &mut words);
        } else {
            word.push(c);
        }
    }
    end_word(&mut word, &mut quoted, &mut words);
    if !words.is_empty() {
        simples.push(Simple {
            words,
            span: start..command.len(),
        });
    }
    simples
}

/// The program a word runs, by its file name, in lowercase.
pub(super) fn program_name(word: &str) -> String {
    word.rsplit(['/', '\\'])
        .next()
        .unwrap_or(word)
        .to_lowercase()
}

/// Programs whose string arguments are command lines themselves.
const SHELLS: [&str; 15] = [
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "eval",
    "cmd",
    "cmd.exe",
    "powershell",
    "powershell.exe",
    "pwsh",
    "pwsh.exe",
    "iex",
    "invoke-expression",
];

/// Programs that run the command their arguments spell, and shell words
/// that a command follows.
const WRAPPERS: [&str; 30] = [
    "sudo", "doas", "env", "timeout", "nice", "nohup", "time", "command", "exec", "xargs",
    "stdbuf", "ionice", "setsid", "chronic", "strace", "ltrace", "unbuffer", "watch", "eval",
    "cmd", "cmd.exe", "then", "do", "else", "if", "elif", "while", "until", "!", "busybox",
];

/// Where in a simple command's `words` the program it runs may be: the
/// first word past `NAME=value` assignments, and, after a wrapper such as
/// `sudo` or `timeout`, every word after it.
pub(super) fn programs(words: &[String]) -> std::ops::Range<usize> {
    let first = words
        .iter()
        .position(|word| {
            !word
                .split_once('=')
                .is_some_and(|(name, _)| !name.is_empty() && !name.contains(['/', '\\']))
        })
        .unwrap_or(words.len());
    let wrapped = words
        .get(first)
        .is_some_and(|word| WRAPPERS.contains(&program_name(word).as_str()));
    first..if wrapped {
        words.len()
    } else {
        (first + 1).min(words.len())
    }
}

/// The `flot` commands in `command` that an agent should leave to the user,
/// as spelled by their verb: `flot init --force`, `flot trust add`, ...
/// A command run by flot counts, and so does a `flot` word anywhere after
/// a wrapper such as `timeout 5` or `sudo -u root`, and in a string a
/// shell is given to run; `echo flot gc` does not.
pub(super) fn flot_verbs(command: &str, language: Shell, depth: u8, found: &mut Vec<String>) {
    for simple in simple_commands(command, language) {
        for at in programs(&simple.words) {
            let name = program_name(&simple.words[at]);
            if name == "flot" || name == "flot.exe" {
                found.extend(refused_verb(&simple.words[at + 1..]));
            }
            if depth < 4 && SHELLS.contains(&name.as_str()) {
                let inner = if ["cmd", "cmd.exe", "sh", "bash", "zsh", "dash", "ksh", "fish"]
                    .contains(&name.as_str())
                    || (name == "eval" && matches!(language, Shell::Posix))
                {
                    Shell::Posix
                } else {
                    Shell::PowerShell
                };
                for argument in &simple.words[at + 1..] {
                    if argument.contains(char::is_whitespace) {
                        flot_verbs(argument, inner, depth + 1, found);
                    }
                }
            }
        }
    }
}

/// The verb of `flot` run with `args`, when it changes whose evidence
/// counts: who is trusted and with what key, which runs this store holds,
/// the flow file, or the revision and flow proofs are held to.
fn refused_verb(args: &[String]) -> Option<String> {
    let mut words: Vec<&str> = Vec::new();
    let mut options: Vec<&str> = Vec::new();
    let mut other_flow = false;
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        at += 1;
        match arg.as_str() {
            "--" => {
                words.extend(args[at..].iter().map(String::as_str));
                break;
            }
            "-f" | "--file" => {
                other_flow = true;
                at += 1;
            }
            _ if arg.starts_with("--file=")
                || (arg.starts_with("-f") && !arg.starts_with("--")) =>
            {
                other_flow = true;
            }
            _ if arg.starts_with('-') => options.push(arg),
            _ => words.push(arg),
        }
    }
    let has = |flag: &str| {
        options
            .iter()
            .any(|option| *option == flag || option.starts_with(&format!("{flag}=")))
    };
    let verb = *words.first()?;
    let refused = match (verb, words.get(1).copied()) {
        ("init", _) if has("--force") => "init --force".to_string(),
        ("trust", Some(action @ ("add" | "remove" | "rotate")))
        | ("key", Some(action @ ("init" | "rotate"))) => format!("{verb} {action}"),
        ("sign" | "import" | "pull", _) => verb.to_string(),
        ("gc", _) if !has("--dry-run") => "gc".to_string(),
        ("run" | "verify" | "diff", _) if has("--proofs-from") => format!("{verb} --proofs-from"),
        ("run" | "verify" | "diff", _) if other_flow => format!("{verb} -f"),
        _ => return None,
    };
    Some(format!("flot {refused}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flot_command_is_found_where_it_runs_and_not_where_it_is_text() {
        let verbs = |command: &str| {
            let mut found = Vec::new();
            flot_verbs(command, Shell::Posix, 0, &mut found);
            found
        };
        for (command, verb) in [
            ("flot gc", "flot gc"),
            ("timeout 5 flot gc", "flot gc"),
            ("FOO=1 ./target/debug/flot sign r", "flot sign"),
            ("bash -c 'flot trust add a b'", "flot trust add"),
            ("true && flot -f other.toml run", "flot run -f"),
            ("flot run --proofs-from=main", "flot run --proofs-from"),
            ("flot init --force", "flot init --force"),
        ] {
            assert_eq!(verbs(command), [verb], "{command}");
        }
        for command in [
            "echo flot gc",
            "git commit -m 'flot gc'",
            "flot gc --dry-run",
            "flot verify",
            "flot trust list",
            "flot init",
        ] {
            assert!(verbs(command).is_empty(), "{command}");
        }
    }
}
