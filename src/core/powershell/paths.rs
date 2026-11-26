//! Paths as the line names them, and .NET methods given one.

use crate::core::shell;

/// The last name in `path`.
pub(super) fn base(path: &str) -> &str {
    let path = path.trim_end_matches(['/', '\\']);
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

pub(super) fn joined(base: &str, name: &str) -> String {
    let trimmed = base.trim_end_matches(['/', '\\']);
    match trimmed {
        "" if !base.is_empty() => format!("/{name}"),
        "" | "." => name.to_string(),
        _ => format!("{trimmed}/{name}"),
    }
}

/// What a `[System.IO]` method does with the arguments it is given.
pub(super) enum Method {
    /// Writes, creates or deletes the arguments at these positions.
    Changes(&'static [usize]),
    /// Opens its first argument with the access its others name.
    Opens,
}

/// A `[System.IO]` type's static method or constructor, `None` for what
/// only reads: `ReadAllText`, `Exists`, `Get*`, `OpenRead`, a reader, or
/// `[IO.Path]`. Any other method of such a type is taken to change its
/// path.
pub(super) fn dotnet(word: &str) -> Option<Method> {
    const READERS: &[&str] = &[
        "path",
        "streamreader",
        "binaryreader",
        "stringreader",
        "stringwriter",
        "memorystream",
        "fileinfo",
        "directoryinfo",
        "driveinfo",
        "filesystemwatcher",
    ];
    const READS: &[&str] = &[
        "read",
        "get",
        "exists",
        "enumerate",
        "openread",
        "opentext",
        "setcurrentdirectory",
        "equals",
        "referenceequals",
        "tostring",
    ];
    let lower = word.to_ascii_lowercase();
    let (class, method) = lower.strip_prefix('[')?.split_once("]::")?;
    let class = class.strip_prefix("system.").unwrap_or(class);
    let class = class.strip_prefix("io.")?;
    if READERS.contains(&class) || READS.iter().any(|read| method.starts_with(read)) {
        return None;
    }
    Some(match (class, method) {
        (_, "open") | ("filestream", "new") => Method::Opens,
        (_, "copy") => Method::Changes(&[1]),
        (_, "move" | "replace") => Method::Changes(&[0, 1, 2]),
        _ => Method::Changes(&[0]),
    })
}

/// Whether a file is opened only to read: an access of `Read`, and no
/// mode that creates, truncates or appends. Without an access, `Open`
/// gives read and write.
pub(super) fn reads_only(words: &[String]) -> bool {
    let words: Vec<String> = words.iter().map(|word| word.to_ascii_lowercase()).collect();
    words
        .iter()
        .any(|word| word.rsplit("::").next() == Some("read"))
        && !words.iter().any(|word| {
            ["write", "create", "truncate", "append"]
                .iter()
                .any(|mode| word.contains(mode))
        })
}

/// The variable `path` starts with that the caller puts in, `$PWD`,
/// `$HOME`, `$env:NAME` or `${env:NAME}`, and the rest; an empty variable
/// for none.
fn leading_variable(path: &str) -> (&str, &str) {
    let name_end = if let Some(inner) = path.strip_prefix("${") {
        inner.find('}').map(|end| (&inner[..end], end + 3))
    } else if let Some(after) = path.strip_prefix('$') {
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(after.len());
        Some((&after[..end], end + 1))
    } else {
        None
    };
    match name_end {
        Some((name, end)) => {
            let lower = name.to_ascii_lowercase();
            let known = matches!(lower.as_str(), "pwd" | "home")
                || lower
                    .strip_prefix("env:")
                    .is_some_and(|name| !name.is_empty());
            let rest = &path[end..];
            if known && (rest.is_empty() || rest.starts_with(['/', '\\'])) {
                (&path[..end], rest)
            } else {
                ("", path)
            }
        }
        None => ("", path),
    }
}

/// A path as PowerShell names it, `/`-separated and past a `FileSystem::`
/// provider; a leading `$PWD`, `$HOME` or environment variable is left for
/// the caller to expand. `None` for what is not a file: `$null`, another
/// variable, or another provider's path such as `Env:X`.
pub(super) fn file_path(path: &str) -> Option<String> {
    let path = match path.split_once("::") {
        Some((provider, rest)) => {
            let provider = provider.rsplit('\\').next().unwrap_or(provider);
            if !provider.eq_ignore_ascii_case("filesystem") {
                return None;
            }
            rest
        }
        None => path,
    };
    let (variable, rest) = leading_variable(path);
    let provider = rest.split_once(':').is_some_and(|(drive, _)| {
        drive.len() > 1 && drive.chars().all(|c| c.is_ascii_alphabetic())
    });
    if path.is_empty() || rest.contains('$') || provider {
        return None;
    }
    Some(format!("{variable}{rest}").replace('\\', "/"))
}

/// `path` as a statement run in `dir` names it, as the shell reader
/// locates one; a path from a drive, as `C:/x`, stays as written.
pub(super) fn located(dir: Option<&str>, path: &str) -> String {
    let drive = path.split_once(':').is_some_and(|(drive, _)| {
        drive.len() == 1 && drive.chars().all(|c| c.is_ascii_alphabetic())
    });
    if drive {
        path.to_string()
    } else {
        shell::located(dir, path)
    }
}
