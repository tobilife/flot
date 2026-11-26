//! The paths a program changes through its arguments.

use super::command::{parse, strings, Parsed};
use super::paths::{base, joined, located};
use super::tables::COMPRESSORS;

/// The paths a program changes through its arguments.
pub(super) fn written(name: &str, args: &[String]) -> Vec<String> {
    let options_end = args.iter().position(|arg| arg == "--");
    let operands: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(index, arg)| options_end.is_some_and(|end| *index > end) || !arg.starts_with('-'))
        .filter(|(index, _)| options_end != Some(*index))
        .map(|(_, arg)| arg.clone())
        .collect();
    // `-i`, `-i.bak`, `--in-place`, yq's `--inplace`, and perl's `-pi -e`.
    let in_place = args.iter().any(|arg| {
        arg.starts_with("-i")
            || arg.starts_with("--in-place")
            || arg == "--inplace"
            || (!arg.starts_with("--") && arg.starts_with('-') && arg.contains('i'))
    });
    match name {
        "rm" | "unlink" | "shred" | "truncate" | "touch" | "chmod" | "tee" | "rmdir" => operands,
        "cp" | "mv" | "ln" | "install" => copied(name, args),
        "sed" | "perl" | "ruby" | "yq" if in_place => operands,
        // gawk's `-i inplace`.
        "awk" | "gawk"
            if args.iter().enumerate().any(|(at, arg)| {
                let library = match arg.as_str() {
                    "-i" | "--include" => args.get(at + 1).map(String::as_str),
                    _ => arg
                        .strip_prefix("--include=")
                        .or_else(|| arg.strip_prefix("-i")),
                };
                library.is_some_and(|library| library.starts_with("inplace"))
            }) =>
        {
            operands
        }
        "dd" => args
            .iter()
            .filter_map(|arg| arg.strip_prefix("of="))
            .map(str::to_string)
            .collect(),
        "git" => git(args),
        "curl" => curl(args),
        "wget" => wget(args),
        "patch" => patch(args),
        // Its last operand is written, and pruned with `--delete`.
        "rsync" => {
            let parsed = parse(
                args,
                "efBTM@",
                &[
                    "exclude",
                    "include",
                    "filter",
                    "files-from",
                    "exclude-from",
                    "include-from",
                    "rsh",
                    "temp-dir",
                    "partial-dir",
                    "backup-dir",
                    "suffix",
                    "chmod",
                    "chown",
                    "log-file",
                    "password-file",
                    "link-dest",
                    "copy-dest",
                    "compare-dest",
                    "max-size",
                    "min-size",
                    "bwlimit",
                    "timeout",
                    "port",
                    "out-format",
                    "rsync-path",
                ],
            );
            if parsed.has(&["n", "dry-run", "list-only"]) || parsed.operands.len() < 2 {
                return Vec::new();
            }
            strings(parsed.operands[parsed.operands.len() - 1..].to_vec())
        }
        "tar" => tar(args),
        // It extracts into `-d DIR` or here.
        "unzip" => {
            let parsed = parse(args, "dPx", &[]);
            if parsed.has(&["l", "t", "v", "p", "c", "z", "Z"]) {
                return Vec::new();
            }
            vec![parsed
                .values(&["d"])
                .last()
                .copied()
                .unwrap_or(".")
                .to_string()]
        }
        "sort" => strings(
            parse(
                args,
                "okStT",
                &[
                    "output",
                    "key",
                    "buffer-size",
                    "field-separator",
                    "temporary-directory",
                    "files0-from",
                    "compress-program",
                    "parallel",
                    "batch-size",
                    "random-source",
                ],
            )
            .values(&["o", "output"]),
        ),
        name if COMPRESSORS.contains(&name) => compressed(args),
        _ => Vec::new(),
    }
}

/// What `cp`, `mv`, `ln` or `install` writes: the target when it may be the
/// file written, each source's name in it when it may be a directory, and
/// what `mv` takes away. A trailing `/`, `.`, `-t DIR` or several sources
/// make the target a directory.
fn copied(name: &str, args: &[String]) -> Vec<String> {
    let parsed = parse(
        args,
        if name == "install" { "tSmog" } else { "tS" },
        &[
            "target-directory",
            "suffix",
            "mode",
            "owner",
            "group",
            "strip-program",
        ],
    );
    if name == "install" && parsed.has(&["d", "directory"]) {
        return strings(parsed.operands);
    }
    let (target, sources, directory) = match parsed.values(&["t", "target-directory"]).last() {
        Some(dir) => (*dir, &parsed.operands[..], true),
        None => {
            let Some((target, sources)) = parsed.operands.split_last() else {
                return Vec::new();
            };
            if sources.is_empty() {
                // `ln -s x` links `x` here; a glob or brace may stand for
                // the sources and the target.
                let mut written = Vec::new();
                if name == "ln" {
                    written.push(base(target).to_string());
                }
                if name == "mv" || target.contains(['*', '?', '[', '{']) {
                    written.push(target.to_string());
                }
                return written;
            }
            let directory =
                target.ends_with('/') || matches!(*target, "." | "..") || sources.len() > 1;
            (*target, sources, directory)
        }
    };
    let mut written = if name == "mv" {
        strings(sources.to_vec())
    } else {
        Vec::new()
    };
    if parsed.has(&["T", "no-target-directory"]) {
        written.push(target.to_string());
        return written;
    }
    if !directory {
        written.push(target.to_string());
    }
    written.extend(sources.iter().map(|source| joined(target, base(source))));
    written
}

/// What a `git` command changes, under its `-C`: its paths, `.` for the
/// whole tree when it rewrites the working tree, or its configuration file.
fn git(args: &[String]) -> Vec<String> {
    let mut dir = String::new();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        match arg.as_str() {
            "-C" => {
                if let Some(next) = args.get(at + 1) {
                    dir = located(Some(&dir), next);
                }
                at += 2;
            }
            "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix"
            | "--config-env" | "--attr-source" => at += 2,
            option if option.starts_with('-') => at += 1,
            _ => break,
        }
    }
    let Some((sub, rest)) = args.get(at..).and_then(<[String]>::split_first) else {
        return Vec::new();
    };
    let whole = || vec![".".to_string()];
    let paths_or_whole = |parsed: Parsed| {
        if parsed.operands.is_empty() {
            whole()
        } else {
            strings(parsed.operands)
        }
    };
    let written = match sub.as_str() {
        "checkout" => {
            let parsed = parse(rest, "bB", &["orphan", "conflict", "pathspec-from-file"]);
            match parsed.dashes {
                Some(start) => strings(parsed.operands[start..].to_vec()),
                None if parsed.has(&["pathspec-from-file"]) => whole(),
                // A revision and paths, or paths alone.
                None if parsed.operands.len() > 1 => strings(parsed.operands),
                // A revision: the tree changes to it.
                None if !parsed.operands.is_empty()
                    || parsed.has(&["f", "force", "m", "merge", "p", "patch"]) =>
                {
                    whole()
                }
                None => Vec::new(),
            }
        }
        "restore" => paths_or_whole(parse(rest, "s", &["source", "pathspec-from-file"])),
        "switch" => {
            let parsed = parse(
                rest,
                "cC",
                &["create", "force-create", "orphan", "conflict"],
            );
            if parsed.operands.is_empty() && parsed.has(&["c", "C", "create", "force-create"]) {
                Vec::new()
            } else {
                whole()
            }
        }
        "stash" => match rest.first().map(String::as_str) {
            Some("list" | "show" | "drop" | "clear" | "create" | "store") => Vec::new(),
            Some("push") => {
                paths_or_whole(parse(&rest[1..], "m", &["message", "pathspec-from-file"]))
            }
            Some(option) if option.starts_with('-') => {
                paths_or_whole(parse(rest, "m", &["message", "pathspec-from-file"]))
            }
            _ => whole(),
        },
        "reset"
            if rest
                .iter()
                .any(|arg| matches!(arg.as_str(), "--hard" | "--merge" | "--keep")) =>
        {
            whole()
        }
        "apply" => {
            let parsed = parse(rest, "pC", &["directory", "exclude", "include"]);
            let only_reads =
                parsed.has(&["check", "stat", "numstat", "summary"]) && !parsed.has(&["apply"]);
            if only_reads || parsed.has(&["cached"]) {
                Vec::new()
            } else {
                whole()
            }
        }
        // Commits brought into the tree, which may change any file; only
        // stopping or resuming one already judged does not.
        "merge" | "pull" | "rebase" | "cherry-pick" | "revert" | "am" => {
            if rest.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "--abort" | "--continue" | "--quit" | "--skip" | "--show-current-patch"
                )
            }) {
                Vec::new()
            } else {
                whole()
            }
        }
        "rm" | "mv" => strings(parse(rest, "", &["pathspec-from-file"]).operands),
        "clean" => {
            let parsed = parse(rest, "e", &["exclude"]);
            if parsed.has(&["n", "dry-run"]) {
                Vec::new()
            } else {
                paths_or_whole(parsed)
            }
        }
        "config" => config(rest),
        _ => Vec::new(),
    };
    written
        .into_iter()
        .map(|path| located(Some(&dir), &path))
        .collect()
}

/// The file `git config` writes, when it sets or removes something.
fn config(args: &[String]) -> Vec<String> {
    let parsed = parse(
        args,
        "f",
        &["file", "blob", "type", "default", "comment", "value"],
    );
    let first = parsed.operands.first().copied();
    let reads = parsed.has(&[
        "get",
        "get-all",
        "get-regexp",
        "get-urlmatch",
        "get-color",
        "get-colorbool",
        "list",
        "l",
        "blob",
    ]) || matches!(first, Some("get" | "list"));
    let writes = parsed.has(&[
        "unset",
        "unset-all",
        "add",
        "replace-all",
        "rename-section",
        "remove-section",
        "edit",
        "e",
    ]) || matches!(
        first,
        Some("set" | "unset" | "rename-section" | "remove-section" | "edit")
    ) || parsed.operands.len() > 1;
    if reads || !writes {
        return Vec::new();
    }
    let file = match parsed.values(&["f", "file"]).last() {
        Some(file) => file,
        None if parsed.has(&["global"]) => "~/.gitconfig",
        None if parsed.has(&["system"]) => "/etc/gitconfig",
        None if parsed.has(&["worktree"]) => ".git/config.worktree",
        None => ".git/config",
    };
    vec![file.to_string()]
}

/// The file name a URL is saved under: its path's last segment.
fn url_name(url: &str) -> Option<&str> {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    let path = url.split_once("://").map_or(url, |(_, rest)| rest);
    let name = path.rsplit_once('/')?.1;
    (!name.is_empty()).then_some(name)
}

/// What `curl` saves: `-o`, `-D`, `-c` and the like, or with `-O` each
/// URL's file name; under `--output-dir`.
fn curl(args: &[String]) -> Vec<String> {
    let parsed = parse(
        args,
        "AbcCdDeEFHKmorTuUwxXyYzQPt",
        &[
            "output",
            "dump-header",
            "cookie-jar",
            "cookie",
            "data",
            "data-raw",
            "data-binary",
            "data-urlencode",
            "form",
            "header",
            "user-agent",
            "user",
            "proxy",
            "request",
            "referer",
            "url",
            "max-time",
            "connect-timeout",
            "retry",
            "config",
            "cert",
            "key",
            "cacert",
            "output-dir",
            "write-out",
            "trace",
            "trace-ascii",
            "stderr",
            "libcurl",
            "etag-save",
            "range",
            "upload-file",
            "time-cond",
            "resolve",
            "limit-rate",
            "json",
        ],
    );
    let dir = parsed
        .values(&["output-dir"])
        .last()
        .copied()
        .unwrap_or_default();
    let mut written: Vec<String> = parsed
        .values(&["o", "output"])
        .into_iter()
        .map(|path| located(Some(dir), path))
        .collect();
    written.extend(strings(parsed.values(&[
        "D",
        "dump-header",
        "c",
        "cookie-jar",
        "trace",
        "trace-ascii",
        "stderr",
        "libcurl",
        "etag-save",
    ])));
    if parsed.has(&["O", "remote-name", "remote-name-all"]) {
        let urls = parsed
            .operands
            .iter()
            .copied()
            .chain(parsed.values(&["url"]));
        written.extend(
            urls.filter_map(url_name)
                .map(|name| located(Some(dir), name)),
        );
    }
    written
}

/// What `wget` saves: `-O`, its logs, or each URL's file name under `-P`.
fn wget(args: &[String]) -> Vec<String> {
    let parsed = parse(
        args,
        "OoaPeiBtTwQUDlARIXn",
        &[
            "output-document",
            "output-file",
            "append-output",
            "directory-prefix",
            "execute",
            "input-file",
            "base",
            "tries",
            "timeout",
            "wait",
            "quota",
            "user-agent",
            "domains",
            "level",
            "accept",
            "reject",
            "header",
            "user",
            "password",
            "post-data",
            "post-file",
            "referer",
            "save-cookies",
            "load-cookies",
        ],
    );
    let mut written =
        strings(parsed.values(&["o", "output-file", "a", "append-output", "save-cookies"]));
    if parsed.has(&["spider"]) {
        return written;
    }
    let documents = parsed.values(&["O", "output-document"]);
    if !documents.is_empty() {
        written.extend(strings(documents));
        return written;
    }
    let dir = parsed
        .values(&["P", "directory-prefix"])
        .last()
        .copied()
        .unwrap_or_default();
    if parsed.has(&["r", "recursive", "m", "mirror"]) {
        written.push(if dir.is_empty() { "." } else { dir }.to_string());
    } else {
        written.extend(
            parsed
                .operands
                .iter()
                .map(|url| located(Some(dir), url_name(url).unwrap_or("index.html"))),
        );
    }
    written
}

/// What `patch` changes: the file it is given, its `-o` and `-r` files,
/// or, when the patch names the files, the whole tree under `-d`.
fn patch(args: &[String]) -> Vec<String> {
    let parsed = parse(
        args,
        "BdDFgioprVYz",
        &[
            "directory",
            "input",
            "output",
            "reject-file",
            "strip",
            "fuzz",
            "prefix",
            "suffix",
            "basename-prefix",
            "ifdef",
            "get",
            "version-control",
            "quoting-style",
            "reject-format",
        ],
    );
    if parsed.has(&["dry-run"]) {
        return Vec::new();
    }
    let dir = parsed
        .values(&["d", "directory"])
        .last()
        .copied()
        .unwrap_or_default();
    let mut written: Vec<String> = parsed
        .values(&["o", "output", "r", "reject-file"])
        .into_iter()
        .map(|path| located(Some(dir), path))
        .collect();
    if parsed.has(&["o", "output"]) {
        return written;
    }
    written.push(match parsed.operands.first() {
        Some(file) => located(Some(dir), file),
        None if dir.is_empty() => ".".to_string(),
        None => dir.to_string(),
    });
    written
}

/// What `tar` changes: the directory it extracts into, or the archive it
/// creates or appends to.
fn tar(args: &[String]) -> Vec<String> {
    let mut args = args.to_vec();
    // `tar xzf a.tgz`: old-style letters without a dash.
    if let Some(first) = args.first_mut() {
        if !first.is_empty() && first.chars().all(|c| c.is_ascii_alphabetic()) {
            first.insert(0, '-');
        }
    }
    let parsed = parse(
        &args,
        "fCbFgHKLNTVXI",
        &[
            "file",
            "directory",
            "blocking-factor",
            "info-script",
            "new-volume-script",
            "listed-incremental",
            "format",
            "starting-file",
            "tape-length",
            "newer",
            "newer-mtime",
            "files-from",
            "label",
            "exclude-from",
            "use-compress-program",
            "exclude",
            "transform",
            "owner",
            "group",
            "mode",
            "mtime",
            "suffix",
            "to-command",
            "strip-components",
        ],
    );
    if parsed.has(&["x", "extract", "get"]) {
        if parsed.has(&["O", "to-stdout", "t", "list"]) {
            return Vec::new();
        }
        let dir = parsed.values(&["C", "directory"]).last().copied();
        return vec![dir.unwrap_or(".").to_string()];
    }
    if parsed.has(&[
        "c",
        "r",
        "u",
        "A",
        "create",
        "append",
        "update",
        "catenate",
        "concatenate",
        "delete",
    ]) {
        return strings(parsed.values(&["f", "file"]));
    }
    Vec::new()
}

/// What a compressor replaces: each file, and the name it decompresses to;
/// nothing but `-o` when it writes to its output, lists or tests.
fn compressed(args: &[String]) -> Vec<String> {
    let parsed = parse(
        args,
        "SToDFCMo",
        &["suffix", "threads", "format", "check", "memlimit", "output"],
    );
    let mut written = strings(parsed.values(&["o", "output"]));
    if parsed.has(&["c", "stdout", "to-stdout", "l", "list", "t", "test"]) {
        return written;
    }
    for file in &parsed.operands {
        written.push(file.to_string());
        let stem = [
            ".gz", ".bz2", ".xz", ".lzma", ".zst", ".lz4", ".br", ".Z", ".z",
        ]
        .iter()
        .find_map(|suffix| file.strip_suffix(suffix));
        written.extend(stem.map(str::to_string));
    }
    written
}
