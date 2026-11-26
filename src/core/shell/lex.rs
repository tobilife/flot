//! Splitting a command into tokens the way `sh` does.

use crate::core::reading::DEPTH;

use super::paths::is_name;
use super::tables::KEYWORDS;
use super::Origin;

/// A word as the line spells it, its quotes taken away, with the commands
/// substituted in it: what each prints goes at its byte offset in `text`,
/// worked out where the word is used, with the variables set by then.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Word {
    pub(super) text: String,
    pub(super) printed: Vec<(usize, String)>,
}

impl Word {
    /// What the command `inner` prints goes here.
    fn print(&mut self, inner: &[char]) {
        self.printed.push((self.text.len(), inner.iter().collect()));
    }

    /// `${..}` as written, a variable this reader puts in or leaves
    /// unknown; the commands substituted in it still run.
    fn expansion(&mut self, expansion: &[char], out: &mut Vec<Token>) {
        out.extend(
            substitutions(&expansion[2..expansion.len() - 1], 0)
                .into_iter()
                .map(Token::Nested),
        );
        self.text.extend(expansion);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Token {
    Word(Word),
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
    /// `;;`, `;&` or `;;&`: the end of a branch of a `case`, where its
    /// next patterns start; elsewhere, as `;`.
    CaseEnd,
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

/// Where the `}` is that closes a `${` opened before `at`, past quotes,
/// escapes and commands substituted in it: a `{` alone opens nothing, as
/// in bash. `None` when none does.
fn braced(chars: &[char], mut at: usize) -> Option<usize> {
    while at < chars.len() {
        match chars[at] {
            '\\' => at += 1,
            // In bash a quote quotes in it, in double quotes too.
            quote @ ('\'' | '"') => {
                at += 1;
                while at < chars.len() && chars[at] != quote {
                    if quote == '"' && chars[at] == '\\' {
                        at += 1;
                    }
                    at += 1;
                }
            }
            '$' if chars.get(at + 1) == Some(&'(') => at = closing(chars, at + 2, 1, usize::MAX),
            '$' if chars.get(at + 1) == Some(&'{') => at = braced(chars, at + 2)?,
            '`' => at = backquoted(chars, at + 1),
            '}' => return Some(at),
            _ => {}
        }
        at += 1;
    }
    None
}

/// Whether a word ends where a command can start: after an operator or a
/// reserved word such as `if`, where `[[` opens a test.
fn starts_command(out: &[Token]) -> bool {
    match out
        .iter()
        .rev()
        .find(|token| !matches!(token, Token::Nested(_)))
    {
        None => true,
        Some(Token::Word(word)) => {
            word.printed.is_empty() && KEYWORDS.contains(&word.text.as_str())
        }
        Some(token) => matches!(
            token,
            Token::Pipe | Token::And | Token::Break | Token::Open | Token::Close | Token::CaseEnd
        ),
    }
}

/// Whether `word`, written just before `<` or `>`, names the stream the
/// redirection takes: a number, as `2>`, or bash's `{NAME}`, as
/// `exec {fd}> log`, which opens a new one.
fn names_stream(word: &Word, quoted: bool) -> bool {
    !quoted
        && word.printed.is_empty()
        && (word.text.chars().all(|c| c.is_ascii_digit())
            || word
                .text
                .strip_prefix('{')
                .and_then(|name| name.strip_suffix('}'))
                .is_some_and(is_name))
}

/// What `$0`, `${0}`, `$BASH_SOURCE`, `${BASH_SOURCE}` or
/// `${BASH_SOURCE[0]}` at `at` in `chars` stands for in code read where
/// `origin` says, and where it ends; `None` for any other `$`, or one
/// `origin` does not know.
fn script_name<'a>(chars: &[char], at: usize, origin: &'a Origin) -> Option<(&'a str, usize)> {
    let rest = &chars[at + 1..chars.len().min(at + 18)];
    let forms: [(&str, &Option<String>); 5] = [
        ("{0}", &origin.zero),
        ("{BASH_SOURCE}", &origin.source),
        ("{BASH_SOURCE[0]}", &origin.source),
        ("0", &origin.zero),
        ("BASH_SOURCE", &origin.source),
    ];
    for (form, value) in forms {
        let length = form.len();
        let matches = rest.len() >= length && rest[..length].iter().copied().eq(form.chars());
        let longer = rest
            .get(length)
            .is_some_and(|c| form == "BASH_SOURCE" && (c.is_ascii_alphanumeric() || *c == '_'));
        if matches && !longer {
            return value.as_deref().map(|value| (value, at + length));
        }
    }
    None
}

/// The tokens of `command`, read where `origin` says: what it knows of
/// `$0` and `$BASH_SOURCE` is put in where the shell expands them, not in
/// single quotes.
pub(super) fn tokens(command: &str, origin: &Origin) -> Vec<Token> {
    let chars: Vec<char> = command.chars().collect();
    let mut out = Vec::new();
    let mut word = Word::default();
    let mut quoted = false;
    let mut heredocs: Vec<Heredoc> = Vec::new();
    // In bash's `[[ .. ]]`, `<`, `>`, `&&`, `||` and parentheses belong to
    // the test: it redirects and separates nothing. It ends at `]]`, or,
    // to see at worst a redirection that is none, at the end of the line.
    let mut testing = false;
    let mut at = 0;
    let end_word =
        |word: &mut Word, quoted: &mut bool, testing: &mut bool, out: &mut Vec<Token>| {
            if !word.text.is_empty() || !word.printed.is_empty() || *quoted {
                let ended = std::mem::take(word);
                if !*quoted && ended.printed.is_empty() {
                    if *testing {
                        *testing = ended.text != "]]";
                    } else {
                        *testing = ended.text == "[[" && starts_command(out);
                    }
                }
                out.push(Token::Word(ended));
            }
            *quoted = false;
        };
    while at < chars.len() {
        let c = chars[at];
        let next = chars.get(at + 1).copied();
        let name = if c == '$' {
            script_name(&chars, at, origin)
        } else {
            None
        };
        match c {
            '\'' => {
                quoted = true;
                at += 1;
                while at < chars.len() && chars[at] != '\'' {
                    word.text.push(chars[at]);
                    at += 1;
                }
            }
            '$' if name.is_some() => {
                if let Some((value, end)) = name {
                    word.text.push_str(value);
                    at = end;
                }
            }
            '"' => {
                quoted = true;
                at += 1;
                while at < chars.len() && chars[at] != '"' {
                    if let Some((value, end)) = script_name(&chars, at, origin) {
                        word.text.push_str(value);
                        at = end + 1;
                        continue;
                    }
                    match chars[at] {
                        // In double quotes a backslash quotes only `$`, a
                        // backquote, `"`, a backslash and a newline;
                        // before anything else it is itself, as in a
                        // Windows path `"C:\Users\me"`.
                        '\\' if matches!(chars.get(at + 1), Some('$' | '`' | '"' | '\\')) => {
                            at += 1;
                            word.text.push(chars[at]);
                        }
                        '\\' if chars.get(at + 1) == Some(&'\n') => at += 1,
                        // A command substituted in double quotes still runs.
                        '$' if chars.get(at + 1) == Some(&'(') => {
                            let close = closing(&chars, at + 2, 1, usize::MAX);
                            let inner = &chars[at + 2..close];
                            out.extend(substituted(inner, 0).into_iter().map(Token::Nested));
                            if arithmetic(inner) {
                                word.text.extend(&chars[at..chars.len().min(close + 1)]);
                            } else {
                                word.print(inner);
                            }
                            at = close;
                        }
                        '$' if chars.get(at + 1) == Some(&'{') => match braced(&chars, at + 2) {
                            Some(close) => {
                                word.expansion(&chars[at..=close], &mut out);
                                at = close;
                            }
                            None => word.text.push('$'),
                        },
                        '`' => {
                            let close = backquoted(&chars, at + 1);
                            out.push(Token::Nested(chars[at + 1..close].iter().collect()));
                            word.print(&chars[at + 1..close]);
                            at = close;
                        }
                        other => word.text.push(other),
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
                        at += escape(&chars[at + 1..], &mut word.text);
                    } else {
                        word.text.push(chars[at]);
                    }
                    at += 1;
                }
            }
            // `$".."` is a double-quoted string.
            '$' if next == Some('"') => {}
            // `${..}` is one expansion, whatever it holds: in `${x//>/<}`
            // nothing is redirected.
            '$' if next == Some('{') => match braced(&chars, at + 2) {
                Some(close) => {
                    word.expansion(&chars[at..=close], &mut out);
                    at = close;
                }
                None => word.text.push('$'),
            },
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
                    word.text.extend(&chars[at..=close]);
                    at = close;
                } else {
                    // What the command substituted prints is part of the
                    // word, which goes on after it.
                    let close = closing(&chars, at + 2, 1, usize::MAX);
                    out.push(Token::Nested(chars[at + 2..close].iter().collect()));
                    word.print(&chars[at + 2..close.min(chars.len())]);
                    at = close;
                }
            }
            '`' => {
                let close = backquoted(&chars, at + 1);
                out.push(Token::Nested(chars[at + 1..close].iter().collect()));
                word.print(&chars[at + 1..close]);
                at = close;
            }
            '\\' => {
                at += 1;
                if at < chars.len() && chars[at] != '\n' {
                    word.text.push(chars[at]);
                }
            }
            // A comment runs to the end of the line.
            '#' if word.text.is_empty() && word.printed.is_empty() && !quoted => {
                while at + 1 < chars.len() && chars[at + 1] != '\n' {
                    at += 1;
                }
            }
            // bash's extended patterns, `!(keep)` or `@(a|b).sh`, match
            // what `*` may: a name that is all of them may be any.
            '!' | '@' | '*' | '+' | '?' if next == Some('(') && !testing => {
                let close = closing(&chars, at + 2, 1, usize::MAX);
                if close < chars.len() {
                    word.text.push('*');
                    at = close;
                } else {
                    word.text.push(c);
                }
            }
            ' ' | '\t' => end_word(&mut word, &mut quoted, &mut testing, &mut out),
            '\n' => {
                end_word(&mut word, &mut quoted, &mut testing, &mut out);
                testing = false;
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
            '|' | '&' | ';' | '(' | ')' | '<' | '>' if testing => word.text.push(c),
            // `NAME=(a b)` sets an array: its words are no command.
            '(' if !quoted
                && word.printed.is_empty()
                && word
                    .text
                    .strip_suffix('=')
                    .is_some_and(|name| is_name(name.strip_suffix('+').unwrap_or(name))) =>
            {
                let close = closing(&chars, at + 1, 1, usize::MAX);
                out.extend(
                    substitutions(&chars[at + 1..close], 0)
                        .into_iter()
                        .map(Token::Nested),
                );
                word.text.extend(&chars[at..chars.len().min(close + 1)]);
                at = close;
            }
            '|' | '&' | ';' | '(' | ')' => {
                if c == '&' && next == Some('>') {
                    end_word(&mut word, &mut quoted, &mut testing, &mut out);
                    at += 1;
                    continue;
                }
                if c == '(' && next == Some('(') && word.text.is_empty() && !quoted {
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
                end_word(&mut word, &mut quoted, &mut testing, &mut out);
                // `;;`, `;&` and `;;&` end a branch of a `case`.
                if c == ';' && matches!(next, Some(';' | '&')) {
                    at += 1;
                    if next == Some(';') && chars.get(at + 1) == Some(&'&') {
                        at += 1;
                    }
                    out.push(Token::CaseEnd);
                    at += 1;
                    continue;
                }
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
                if names_stream(&word, quoted) {
                    word = Word::default();
                } else {
                    end_word(&mut word, &mut quoted, &mut testing, &mut out);
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
                if names_stream(&word, quoted) {
                    word = Word::default();
                } else {
                    end_word(&mut word, &mut quoted, &mut testing, &mut out);
                }
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
            _ => word.text.push(c),
        }
        at += 1;
    }
    end_word(&mut word, &mut quoted, &mut testing, &mut out);
    out
}

pub(super) fn is_word(token: &Token) -> bool {
    matches!(token, Token::Word(_))
}
