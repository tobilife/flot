//! What a statement pipes on, the variables a line sets, and the location.

use std::collections::HashSet;

use crate::core::reading::RUNS;
use crate::core::shell;

use super::cmdlets::{command_name, Argument, Bound, Piped, PATH_PARAMETERS};
use super::paths::file_path;

/// What a statement pipes on that names paths: literal strings
/// (`'a', 'b' |`), what `Write-Output` is given, the items `Get-Item`
/// and `Get-ChildItem` are given (`.` for none), or, past a cmdlet that
/// only filters or orders what it is piped, that.
pub(super) fn piped(arguments: &[Argument], input: &Piped) -> Piped {
    let Some((first, _)) = arguments.first().and_then(|argument| argument.first()) else {
        return Piped::default();
    };
    if arguments.iter().flatten().all(|(_, quoted)| *quoted) {
        return Piped {
            paths: arguments
                .iter()
                .flatten()
                .map(|(word, _)| word.clone())
                .collect(),
            items: false,
        };
    }
    let bound = Bound::new(&arguments[1..]);
    let (paths, items) = match command_name(first).as_str() {
        "write-output" | "echo" | "write" => (bound.positional.concat(), false),
        "get-item" | "gi" => (bound.values(PATH_PARAMETERS, 0), true),
        "get-childitem" | "gci" | "ls" | "dir" => {
            let paths = bound.values(PATH_PARAMETERS, 0);
            if paths.is_empty() {
                (vec![".".to_string()], true)
            } else {
                (paths, true)
            }
        }
        name if FILTERS.contains(&name) => (input.paths.clone(), input.items),
        _ => (Vec::new(), false),
    };
    Piped { paths, items }
}

/// Cmdlets that pass on some of what they are piped, in some order.
pub(super) const FILTERS: &[&str] = &[
    "where-object",
    "where",
    "?",
    "sort-object",
    "sort",
    "select-object",
    "select",
];
/// Cmdlets whose script block runs for each item piped in, as `$_`.
pub(super) const FOR_EACH: &[&str] = &[
    "foreach-object",
    "%",
    "foreach",
    "where-object",
    "where",
    "?",
];
/// Cmdlets that change the location the statements after them run in.
const LOCATIONS: &[&str] = &[
    "set-location",
    "sl",
    "cd",
    "chdir",
    "push-location",
    "pushd",
    "pop-location",
    "popd",
];

/// The variables a command line has set: each name, lowercase, with the
/// values it may hold.
#[derive(Default)]
pub(super) struct Variables(Vec<(String, Vec<String>)>);

impl Variables {
    /// Records what a statement of `arguments` sets: `$name = 'value'`,
    /// `$name='value'` or an array of strings. A variable set to anything
    /// else is forgotten.
    pub(super) fn assign(&mut self, arguments: &[Argument]) {
        let Some(((first, quoted), joined)) =
            arguments.first().and_then(|first| first.split_first())
        else {
            return;
        };
        let Some(first) = first.strip_prefix('$') else {
            return;
        };
        // The variable, the operator, and the words it is set to.
        let (name, operator, values): (&str, &str, Vec<(String, bool)>) =
            match first.split_once('=') {
                // `$name='value'` is one word, which the quotes made.
                Some((name, value)) => (
                    name,
                    "=",
                    std::iter::once((value.to_string(), *quoted))
                        .chain(joined.iter().cloned())
                        .chain(arguments[1..].iter().flatten().cloned())
                        .collect(),
                ),
                None => match arguments.get(1).map(Vec::as_slice) {
                    Some([(operator, false)])
                        if !*quoted && joined.is_empty() && operator.ends_with('=') =>
                    {
                        (
                            first,
                            operator.as_str(),
                            arguments[2..].iter().flatten().cloned().collect(),
                        )
                    }
                    _ => return,
                },
            };
        let name = name.to_ascii_lowercase();
        let name = ["script:", "global:", "local:", "private:"]
            .iter()
            .find_map(|scope| name.strip_prefix(scope))
            .unwrap_or(&name)
            .to_string();
        // `$x+=` and the like change it in ways this reader does not
        // follow.
        if name.ends_with(['+', '-', '*', '/', '%', '?']) {
            return;
        }
        self.0.retain(|(known, _)| *known != name);
        let literal =
            operator == "=" && !values.is_empty() && values.iter().all(|(_, quoted)| *quoted);
        if literal {
            self.0
                .push((name, values.into_iter().map(|(word, _)| word).collect()));
        }
    }

    /// `word` with each `$name` or `${name}` the line set put in, and `$_`,
    /// `$PSItem` and their properties as `item`: one text for each value
    /// they may hold, as many as [`RUNS`].
    pub(super) fn substituted(&self, word: &str, item: Option<&[String]>) -> Vec<String> {
        if !word.contains('$') {
            return vec![word.to_string()];
        }
        let mut done = vec![String::new()];
        let mut rest = word;
        while let Some(at) = rest.find('$') {
            for text in &mut done {
                text.push_str(&rest[..at]);
            }
            let after = &rest[at + 1..];
            let (name, mut used) = match after.strip_prefix('{') {
                Some(inner) => match inner.find('}') {
                    Some(end) => (&inner[..end], end + 2),
                    None => ("", 0),
                },
                None => {
                    let end = after
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                        .unwrap_or(after.len());
                    (&after[..end], end)
                }
            };
            let lower = name.to_ascii_lowercase();
            let values = if lower == "_" || lower == "psitem" {
                // An item's `.FullName` or `.Name` is the item, as near as
                // this reader can name it.
                let property = after[used..]
                    .strip_prefix('.')
                    .map(|rest| {
                        rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                            .unwrap_or(rest.len())
                    })
                    .filter(|length| *length > 0);
                if let Some(length) = property {
                    used += 1 + length;
                }
                item
            } else {
                self.0
                    .iter()
                    .find(|(known, _)| *known == lower)
                    .map(|(_, values)| values.as_slice())
            };
            match values {
                Some(values) if !values.is_empty() => {
                    done = done
                        .iter()
                        .flat_map(|text| values.iter().map(move |value| format!("{text}{value}")))
                        .take(RUNS)
                        .collect();
                    rest = &after[used..];
                }
                _ => {
                    for text in &mut done {
                        text.push('$');
                    }
                    rest = after;
                }
            }
        }
        for text in &mut done {
            text.push_str(rest);
        }
        done
    }
}

/// Moves `dirs` past a statement of `arguments` that changes the location:
/// to the directory it names, or to one this reader cannot name. Only after
/// `Set-Location x &&` do the statements surely run there.
pub(super) fn moved(dirs: &mut Vec<Option<String>>, arguments: &[Argument], certain: bool) {
    let Some((first, _)) = arguments.first().and_then(|argument| argument.first()) else {
        return;
    };
    let name = command_name(first);
    if !LOCATIONS.contains(&name.as_str()) {
        return;
    }
    let target = if matches!(name.as_str(), "pop-location" | "popd") {
        None
    } else {
        Bound::new(&arguments[1..])
            .values(PATH_PARAMETERS, 0)
            .first()
            .filter(|target| target.as_str() != "-")
            .and_then(|target| file_path(target))
    };
    let next: Vec<Option<String>> = dirs
        .iter()
        .map(|dir| {
            let target = target.as_deref()?;
            let drive = target
                .split_once(':')
                .is_some_and(|(drive, _)| drive.len() == 1);
            if drive || target.starts_with(['/', '~', '$']) {
                Some(target.to_string())
            } else {
                dir.as_deref().map(|dir| shell::joined(dir, target))
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
