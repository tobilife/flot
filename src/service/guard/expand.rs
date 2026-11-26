//! Words as the shell expands them against the disk and the environment:
//! braces, `~`, variables and wildcards.

use std::path::{Path, PathBuf};

use super::paths::FOLDS_CASE;
use super::Shell;

/// The user's home directory, which `~` and `$HOME` name.
pub(super) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// `path` with a leading `~` as the home directory, as a shell expands it.
pub fn tilde(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), home_dir()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(path),
    }
}

/// How many paths one written word may expand to.
const EXPANSION_LIMIT: usize = 1000;

/// `word` with its `{a,b}` groups expanded as bash expands them, up to
/// [`EXPANSION_LIMIT`] words.
fn braces(word: &str) -> Vec<String> {
    let mut done = Vec::new();
    let mut pending = vec![word.to_string()];
    while let Some(word) = pending.pop() {
        match brace_group(&word) {
            Some((open, commas, close)) if done.len() + pending.len() < EXPANSION_LIMIT => {
                let mut edges = vec![open];
                edges.extend(commas);
                edges.push(close);
                for pair in edges.windows(2) {
                    pending.push(format!(
                        "{}{}{}",
                        &word[..open],
                        &word[pair[0] + 1..pair[1]],
                        &word[close + 1..]
                    ));
                }
            }
            _ => done.push(word),
        }
    }
    done
}

/// The first `{` whose matching `}` encloses a comma at its own level, not
/// a `${NAME}`: where it opens, its commas, and where it closes.
fn brace_group(word: &str) -> Option<(usize, Vec<usize>, usize)> {
    let bytes = word.as_bytes();
    for open in 0..bytes.len() {
        if bytes[open] != b'{' || (open > 0 && bytes[open - 1] == b'$') {
            continue;
        }
        let mut depth = 0;
        let mut commas = Vec::new();
        for (close, byte) in bytes.iter().enumerate().skip(open + 1) {
            match byte {
                b'{' => depth += 1,
                b'}' if depth == 0 => {
                    if !commas.is_empty() {
                        return Some((open, commas, close));
                    }
                    break;
                }
                b'}' => depth -= 1,
                b',' if depth == 0 => commas.push(close),
                _ => {}
            }
        }
    }
    None
}

/// The value of the environment variable `name`, which the shell the
/// agent runs is given too; `None` for one not set or not a name.
fn environment(name: &str) -> Option<String> {
    let mut chars = name.chars();
    let is_name = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !is_name {
        return None;
    }
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

/// `word` with `~`, `$HOME`, `$PWD` and the other environment variables
/// put in, `${NAME}` too (in PowerShell, `$HOME`, `$PWD` and `$env:NAME`
/// in any case, `$env:HOME` and `$env:USERPROFILE` as the home directory
/// where they are not set): as the path it names, and as a wildcard
/// pattern with what was put in escaped.
pub(super) fn variables(word: &str, dir: &Path, language: Shell) -> (String, String) {
    let home = home_dir().map(|home| home.to_string_lossy().into_owned());
    let here = dir.to_string_lossy().into_owned();
    let mut literal = String::new();
    let mut pattern = String::new();
    let put = |value: &str, literal: &mut String, pattern: &mut String| {
        literal.push_str(value);
        pattern.push_str(&glob::Pattern::escape(value));
    };
    let mut rest = word;
    if let (Some(home), Some(after)) = (&home, word.strip_prefix('~')) {
        if after.is_empty() || after.starts_with('/') {
            put(home, &mut literal, &mut pattern);
            rest = after;
        }
    }
    while let Some(at) = rest.find('$') {
        literal.push_str(&rest[..at]);
        pattern.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (name, used) = match after.strip_prefix('{') {
            Some(inner) => match inner.find('}') {
                Some(end) => (&inner[..end], end + 2),
                None => ("", 0),
            },
            None => {
                let end = after
                    .find(|c: char| {
                        !(c.is_ascii_alphanumeric()
                            || c == '_'
                            || (c == ':' && matches!(language, Shell::PowerShell)))
                    })
                    .unwrap_or(after.len());
                (&after[..end], end)
            }
        };
        let value = match language {
            Shell::Posix => match name {
                "HOME" => home.clone(),
                "PWD" => Some(here.clone()),
                _ => environment(name),
            },
            Shell::PowerShell => match name.to_lowercase().as_str() {
                "home" => home.clone(),
                "pwd" => Some(here.clone()),
                lower => lower.strip_prefix("env:").and_then(|_| {
                    environment(&name[4..]).or_else(|| {
                        matches!(lower, "env:home" | "env:userprofile")
                            .then(|| home.clone())
                            .flatten()
                    })
                }),
            },
        };
        match value {
            Some(value) => {
                put(&value, &mut literal, &mut pattern);
                rest = &after[used..];
            }
            None => {
                literal.push('$');
                pattern.push('$');
                rest = after;
            }
        }
    }
    literal.push_str(rest);
    pattern.push_str(rest);
    (literal, pattern)
}

/// The words a written `word` stands for before variables are put in:
/// its braces expanded in a POSIX shell; in PowerShell on another system,
/// with `\` as a separator.
pub(super) fn alternatives(word: &str, language: Shell) -> Vec<String> {
    match language {
        Shell::Posix => braces(word),
        Shell::PowerShell if cfg!(not(windows)) => vec![word.replace('\\', "/")],
        Shell::PowerShell => vec![word.to_string()],
    }
}

/// A relative wildcard `pattern` as one from `dir`.
pub(super) fn anchored(dir: &Path, pattern: &str) -> String {
    format!(
        "{}/{pattern}",
        glob::Pattern::escape(&dir.to_string_lossy())
    )
}

/// How a shell matches a wildcard against paths.
pub(super) fn wildcard_match() -> glob::MatchOptions {
    glob::MatchOptions {
        case_sensitive: !FOLDS_CASE,
        require_literal_separator: true,
        require_literal_leading_dot: true,
    }
}

/// The paths a written `word` names from `dir`, expanded as the shell
/// would: braces (in a POSIX shell), then `~` and variables, then
/// wildcards against the disk. A wildcard that matches nothing is the word
/// itself.
pub(super) fn expand(dir: &Path, word: &str, language: Shell) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for word in alternatives(word, language) {
        let (literal, pattern) = variables(&word, dir, language);
        if literal.contains(['*', '?', '[']) {
            let pattern = if Path::new(&literal).is_absolute() {
                pattern
            } else {
                anchored(dir, &pattern)
            };
            let matched: Vec<PathBuf> = glob::glob_with(&pattern, wildcard_match())
                .map(|found| found.flatten().take(EXPANSION_LIMIT).collect())
                .unwrap_or_default();
            if !matched.is_empty() {
                paths.extend(matched);
                continue;
            }
        }
        paths.push(dir.join(literal));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braces_expand_as_bash_expands_them() {
        let mut found = braces("{check,tests/accept}.sh");
        found.sort();
        assert_eq!(found, ["check.sh", "tests/accept.sh"]);
        let mut found = braces("a{b,{c,d}}e");
        found.sort();
        assert_eq!(found, ["abe", "ace", "ade"]);
        assert_eq!(braces("${HOME}/x"), ["${HOME}/x"]);
        assert_eq!(braces("{solo}.sh"), ["{solo}.sh"]);
        assert!(braces(&"{a,b}".repeat(20)).len() <= super::EXPANSION_LIMIT + 2);
    }
}
