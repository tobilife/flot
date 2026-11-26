//! Splitting a command into tokens the way PowerShell does.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Token {
    /// A word, and whether any of it was quoted.
    Word(String, bool),
    /// `,`: the words on both sides are one array argument.
    Comma,
    /// `|`: the next command reads this one's output.
    Pipe,
    /// `;`, `||` or a newline.
    Break,
    /// `&&`: the next statement runs only if this one succeeded.
    And,
    /// `(`, `{`, `$(`, `@(` or `@{`: a nested expression or script block.
    Open,
    /// `)` or `}`.
    Close,
    /// `>`, `>>`, `2>`, `*>`: the next word is written.
    WriteTo,
}

pub(super) fn tokens(command: &str) -> Vec<Token> {
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
                    out.push(Token::And);
                }
                // Alone, the call operator: the next word is the command.
            }
            ',' => {
                end_word(&mut word, &mut quoted, &mut out);
                out.push(Token::Comma);
            }
            // `${name}` is a variable, whatever its name holds.
            '$' if next == Some('{') => {
                let close = chars[at..]
                    .iter()
                    .position(|c| *c == '}')
                    .map_or(chars.len() - 1, |offset| at + offset);
                word.extend(&chars[at..=close]);
                at = close;
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
