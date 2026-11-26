//! What git is asked, to learn which files a whole-tree git command would
//! change.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// What git is asked, to learn which files a command would change.
enum GitAsk {
    /// The changes in the working tree and index, and untracked files too.
    Status { untracked: bool },
    /// The files that may change, for each way they may: those that differ
    /// in every one of its comparisons, each of two revisions or of a
    /// revision (or, with none, the index) and the working tree. A three-way
    /// merge, for one, changes only what both sides changed.
    Diff(Vec<Vec<Vec<String>>>),
    /// The files these patches touch, with the options that place them.
    Apply(Vec<String>),
    /// What `git clean` with these options would remove.
    Clean(Vec<String>),
    /// Nothing git can answer, and why.
    Unknown(&'static str),
}

/// The operands and options of a git subcommand's `args`, past the options
/// in `valued` that take the next word as their value, and the paths after
/// `--`, if any.
fn git_operands<'a>(
    args: &'a [String],
    valued: &[&str],
) -> (Vec<&'a str>, Vec<&'a str>, Option<Vec<&'a str>>) {
    let mut operands = Vec::new();
    let mut options = Vec::new();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        at += 1;
        if arg == "--" {
            let paths = args[at..].iter().map(String::as_str).collect();
            return (operands, options, Some(paths));
        }
        if arg.starts_with('-') && arg.len() > 1 {
            options.push(arg.as_str());
            if valued.contains(&arg.as_str()) {
                at += 1;
            }
        } else {
            operands.push(arg.as_str());
        }
    }
    (operands, options, None)
}

fn stash_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, _) = git_operands(args, &["-m", "--message"]);
    let untracked = options
        .iter()
        .any(|option| matches!(*option, "-u" | "--include-untracked" | "-a" | "--all"));
    match operands.first().copied() {
        None | Some("push" | "save") => Some(GitAsk::Status { untracked }),
        Some("pop" | "apply") => {
            let entry = operands.get(1).copied().unwrap_or("stash@{0}");
            Some(GitAsk::Diff(vec![vec![
                revisions(&[&format!("{entry}^1"), entry]),
                revisions(&["HEAD", entry]),
            ]]))
        }
        Some("branch") => Some(GitAsk::Unknown(
            "`git stash branch` checks out another revision",
        )),
        // list, show, drop, clear, create, store: the working tree stays.
        _ => None,
    }
}

fn reset_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, _) = git_operands(args, &[]);
    if !options
        .iter()
        .any(|option| matches!(*option, "--hard" | "--keep" | "--merge"))
    {
        return None;
    }
    let revision = operands.first().copied().unwrap_or("HEAD");
    Some(GitAsk::Diff(vec![vec![revisions(&[revision])]]))
}

/// One comparison `git diff` makes.
fn revisions(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// `checkout` or `switch` to a revision. With paths, the command line names
/// what it writes.
fn switch_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, paths) =
        git_operands(args, &["-b", "-B", "-c", "-C", "--orphan", "--conflict"]);
    if options
        .iter()
        .any(|option| option.starts_with("--pathspec-from-file"))
    {
        return Some(GitAsk::Unknown("it reads its paths from a file"));
    }
    if paths.is_some() || operands.len() > 1 {
        return None;
    }
    let revision = operands.first().copied().map(|revision| match revision {
        "-" => "@{-1}",
        revision => revision,
    });
    // `-p` takes back what the working tree changed from the index, or
    // from the revision.
    if options
        .iter()
        .any(|option| matches!(*option, "-p" | "--patch"))
    {
        return Some(GitAsk::Diff(vec![vec![revisions(
            &revision.into_iter().collect::<Vec<_>>(),
        )]]));
    }
    // Local changes stay across a switch unless it is forced.
    let force = options
        .iter()
        .any(|option| matches!(*option, "-f" | "--force" | "--discard-changes"));
    let revision = match revision {
        Some(revision) => revision,
        None if force => "HEAD",
        None => return None,
    };
    Some(GitAsk::Diff(vec![vec![if force {
        revisions(&[revision])
    } else {
        revisions(&["HEAD", revision])
    }]]))
}

/// Whether `options` go on with, or back out of, a merge, rebase,
/// cherry-pick, revert or `am` that already started; that changes no more
/// than starting it did, or takes it back.
fn resumes(options: &[&str]) -> bool {
    options
        .iter()
        .any(|option| matches!(*option, "--abort" | "--continue" | "--quit" | "--skip"))
}

/// `merge` of revisions, by default the upstream: what each brings since
/// it and `HEAD` parted.
fn merge_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, _) = git_operands(
        args,
        &[
            "-m",
            "-F",
            "--file",
            "-s",
            "--strategy",
            "-X",
            "--strategy-option",
            "--into-name",
            "--cleanup",
        ],
    );
    if resumes(&options) {
        return None;
    }
    let revisions = if operands.is_empty() {
        vec!["@{upstream}"]
    } else {
        operands
    };
    Some(GitAsk::Diff(
        revisions
            .into_iter()
            .map(|revision| {
                vec![
                    vec![format!("HEAD...{revision}")],
                    self::revisions(&["HEAD", revision]),
                ]
            })
            .collect(),
    ))
}

/// `rebase`: what differs between the branch and where it lands, and what
/// both the commits it moves and that place changed, which may conflict;
/// and, when it names a branch, what switching to it changes.
fn rebase_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, _) = git_operands(
        args,
        &[
            "--onto",
            "-s",
            "--strategy",
            "-X",
            "--strategy-option",
            "-x",
            "--exec",
        ],
    );
    if resumes(&options)
        || options.contains(&"--edit-todo")
        || options.contains(&"--show-current-patch")
    {
        return None;
    }
    if options.iter().any(|option| {
        matches!(*option, "-x" | "--root" | "-i" | "--interactive") || option.starts_with("--exec")
    }) {
        return Some(GitAsk::Unknown(
            "an interactive rebase, or one that runs commands, does what it is told as it goes",
        ));
    }
    let upstream = operands.first().copied().unwrap_or("@{upstream}");
    let branch = operands.get(1).copied().unwrap_or("HEAD");
    let onto = args
        .iter()
        .position(|arg| arg == "--onto")
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
        .or_else(|| args.iter().find_map(|arg| arg.strip_prefix("--onto=")))
        .unwrap_or(upstream);
    let mut changes = vec![
        vec![revisions(&[branch, onto])],
        vec![
            vec![format!("{upstream}...{branch}")],
            vec![format!("{branch}...{onto}")],
        ],
    ];
    if branch != "HEAD" {
        changes.push(vec![revisions(&["HEAD", branch])]);
    }
    Some(GitAsk::Diff(changes))
}

/// `cherry-pick` or, with `revert`, the reverse of commits, or of ranges
/// of them: what each changes that `HEAD` does not already hold.
fn pick_changes(args: &[String], revert: bool) -> Option<GitAsk> {
    let (operands, options, _) = git_operands(
        args,
        &[
            "-m",
            "--mainline",
            "-s",
            "--strategy",
            "-X",
            "--strategy-option",
            "--cleanup",
        ],
    );
    if resumes(&options) {
        return None;
    }
    if options.contains(&"--stdin") || operands.is_empty() {
        return Some(GitAsk::Unknown("it reads its commits from standard input"));
    }
    let parent = args
        .iter()
        .position(|arg| arg == "-m" || arg == "--mainline")
        .and_then(|at| args.get(at + 1))
        .map_or("1", String::as_str);
    Some(GitAsk::Diff(
        operands
            .into_iter()
            .map(|operand| match operand.split_once("..") {
                Some((from, to)) => {
                    let to = to.trim_start_matches('.');
                    vec![vec![format!(
                        "{}...{}",
                        if from.is_empty() { "HEAD" } else { from },
                        if to.is_empty() { "HEAD" } else { to }
                    )]]
                }
                None => {
                    let before = format!("{operand}^{parent}");
                    let after = if revert { before.as_str() } else { operand };
                    vec![revisions(&[&before, operand]), revisions(&["HEAD", after])]
                }
            })
            .collect(),
    ))
}

/// `am` of mailboxes: the files their patches touch.
fn am_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, _) = git_operands(args, &[]);
    if resumes(&options) || options.contains(&"--show-current-patch") {
        return None;
    }
    if operands.is_empty() {
        return Some(GitAsk::Unknown("it reads the patches from standard input"));
    }
    // The options `git apply` shares place the paths.
    let mut ask: Vec<String> = options
        .into_iter()
        .filter(|option| {
            (option.starts_with("-p") && option.len() > 2)
                || option.starts_with("--directory=")
                || option.starts_with("--exclude=")
                || option.starts_with("--include=")
        })
        .map(str::to_string)
        .collect();
    ask.extend(operands.into_iter().map(str::to_string));
    Some(GitAsk::Apply(ask))
}

/// `clean`, unless it only says what it would remove.
fn clean_changes(args: &[String]) -> Option<GitAsk> {
    let (operands, options, paths) = git_operands(args, &["-e", "--exclude"]);
    if options
        .iter()
        .any(|option| matches!(*option, "-n" | "--dry-run"))
    {
        return None;
    }
    // The options that choose what is removed stay; those that force or
    // ask go, and `--dry-run` takes their place.
    let mut ask = vec!["--dry-run".to_string()];
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        at += 1;
        match arg.as_str() {
            "--" => break,
            "-e" | "--exclude" => {
                ask.push(arg.clone());
                ask.extend(args.get(at).cloned());
                at += 1;
            }
            "-d" | "-x" | "-X" => ask.push(arg.clone()),
            _ if arg.starts_with("--exclude=") || arg.starts_with("-e") && arg.len() > 2 => {
                ask.push(arg.clone())
            }
            // Bundled short options, such as `-fdx`.
            _ if arg.starts_with('-') && !arg.starts_with("--") => {
                for flag in arg[1..]
                    .chars()
                    .filter(|flag| matches!(flag, 'd' | 'x' | 'X'))
                {
                    ask.push(format!("-{flag}"));
                }
            }
            _ => {}
        }
    }
    ask.push("--".to_string());
    ask.extend(
        operands
            .into_iter()
            .chain(paths.unwrap_or_default())
            .map(str::to_string),
    );
    Some(GitAsk::Clean(ask))
}

fn apply_changes(args: &[String]) -> GitAsk {
    let (operands, options, paths) = git_operands(args, &[]);
    let patches: Vec<&str> = operands
        .into_iter()
        .chain(paths.unwrap_or_default())
        .collect();
    if patches.is_empty() || patches.contains(&"-") {
        return GitAsk::Unknown("it reads the patch from standard input");
    }
    // The options that place the paths stay; those that print or apply
    // instead go.
    let mut ask: Vec<String> = options
        .into_iter()
        .filter(|option| {
            !matches!(
                *option,
                "--stat"
                    | "--numstat"
                    | "--summary"
                    | "--check"
                    | "--apply"
                    | "-v"
                    | "--verbose"
                    | "--index"
                    | "--cached"
                    | "-3"
                    | "--3way"
                    | "--reject"
            )
        })
        .map(str::to_string)
        .collect();
    ask.extend(patches.into_iter().map(str::to_string));
    GitAsk::Apply(ask)
}

/// What a whole-tree git command would change: `git` run in `dir` with
/// `args`, the words after `git`. `stash`, `reset --hard`, `checkout` or
/// `switch` to a revision, `checkout -f` or `-p`, `apply`, `am`, `merge`,
/// `rebase`, `cherry-pick`, `revert` and `clean` change files the command
/// line does not name, so git is asked which. `None` for any other git
/// command, whose files the line names itself; an error when git cannot
/// say, as for `pull`, which fetches first: a change the guard cannot rule
/// out.
pub(super) fn git_would_change(dir: &Path, args: &[String]) -> Option<Result<Vec<PathBuf>>> {
    // The options that choose the repository and its working tree are
    // passed on; `-c` and the like are not, so a command line cannot
    // configure the git the guard runs.
    let mut place: Vec<String> = Vec::new();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        match arg.as_str() {
            "-C" | "--git-dir" | "--work-tree" => {
                place.push(arg.clone());
                place.extend(args.get(at + 1).cloned());
                at += 2;
            }
            "-c" | "--namespace" | "--config-env" | "--super-prefix" => at += 2,
            _ if arg.starts_with("--git-dir=") || arg.starts_with("--work-tree=") => {
                place.push(arg.clone());
                at += 1;
            }
            _ if arg.starts_with('-') => at += 1,
            _ => break,
        }
    }
    let (command, rest) = args.get(at..)?.split_first()?;
    let ask = match command.as_str() {
        "stash" => stash_changes(rest)?,
        "reset" => reset_changes(rest)?,
        "checkout" | "switch" => switch_changes(rest)?,
        "apply" => apply_changes(rest),
        "merge" => merge_changes(rest)?,
        "rebase" => rebase_changes(rest)?,
        "cherry-pick" => pick_changes(rest, false)?,
        "revert" => pick_changes(rest, true)?,
        "am" => am_changes(rest)?,
        "clean" => clean_changes(rest)?,
        "pull" => GitAsk::Unknown(
            "`git pull` fetches first; run `git fetch`, then `git merge` or `git rebase`, which can be judged",
        ),
        _ => return None,
    };
    Some(ask_git(dir, &place, &ask))
}

fn ask_git(dir: &Path, place: &[String], ask: &GitAsk) -> Result<Vec<PathBuf>> {
    if let GitAsk::Unknown(why) = ask {
        bail!("{why}");
    }
    let git = |args: &[&str]| -> Result<Vec<u8>> {
        let output = std::process::Command::new("git")
            .args(["-c", "core.fsmonitor=false", "-C"])
            .arg(dir)
            .args(place)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(std::process::Stdio::null())
            .output()
            .context("cannot run git")?;
        if !output.status.success() {
            let said = String::from_utf8_lossy(&output.stderr);
            bail!(
                "{}",
                said.lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("git failed")
                    .trim()
            );
        }
        Ok(output.stdout)
    };
    // `git apply` also patches outside a repository, from where it runs.
    let top = match (git(&["rev-parse", "--show-toplevel"]), ask) {
        (Ok(top), _) => PathBuf::from(String::from_utf8_lossy(&top).trim()),
        (Err(_), GitAsk::Apply(_)) => dir.to_path_buf(),
        (Err(error), _) => return Err(error),
    };
    let names = match ask {
        GitAsk::Status { untracked } => status_paths(&git(&[
            "status",
            "--porcelain",
            "-z",
            if *untracked {
                "--untracked-files=all"
            } else {
                "--untracked-files=no"
            },
        ])?),
        GitAsk::Diff(changes) => {
            let mut names: Vec<String> = Vec::new();
            for comparisons in changes {
                let mut common: Option<Vec<String>> = None;
                for revisions in comparisons {
                    let mut args = vec!["diff", "--name-only", "--no-renames", "-z"];
                    args.extend(revisions.iter().map(String::as_str));
                    args.push("--");
                    let differ: Vec<String> = git(&args)?
                        .split(|byte| *byte == 0)
                        .filter(|name| !name.is_empty())
                        .map(|name| String::from_utf8_lossy(name).into_owned())
                        .collect();
                    common = Some(match common {
                        None => differ,
                        Some(known) => known
                            .into_iter()
                            .filter(|name| differ.contains(name))
                            .collect(),
                    });
                }
                names.extend(common.unwrap_or_default());
            }
            names
        }
        // Named from where git runs, not from the top.
        GitAsk::Clean(options) => {
            let mut args = vec!["-c", "core.quotePath=false", "clean"];
            args.extend(options.iter().map(String::as_str));
            let mut cwd = dir.to_path_buf();
            for pair in place.windows(2) {
                if pair[0] == "-C" {
                    cwd = cwd.join(&pair[1]);
                }
            }
            let mut removed = Vec::new();
            for line in String::from_utf8_lossy(&git(&args)?).lines() {
                let Some(name) = line.strip_prefix("Would remove ") else {
                    continue;
                };
                if name.starts_with('"') {
                    bail!("git quotes the name {name}");
                }
                removed.push(cwd.join(name));
            }
            return Ok(removed);
        }
        GitAsk::Apply(patches) => {
            let mut args = vec!["apply", "--numstat", "-z"];
            args.extend(patches.iter().map(String::as_str));
            numstat_paths(&git(&args)?)
        }
        GitAsk::Unknown(_) => Vec::new(),
    };
    Ok(names.into_iter().map(|name| top.join(name)).collect())
}

/// The paths in `git status --porcelain -z`: each entry's, and a rename's
/// source as well.
fn status_paths(out: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut records = out
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        paths.push(String::from_utf8_lossy(&record[3..]).into_owned());
        if matches!(record[0], b'R' | b'C') {
            paths.extend(
                records
                    .next()
                    .map(|from| String::from_utf8_lossy(from).into_owned()),
            );
        }
    }
    paths
}

/// The paths in `git apply --numstat -z`: `added TAB deleted TAB path`, or
/// for a rename an empty path followed by both names.
fn numstat_paths(out: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut fields = out.split(|byte| *byte == 0);
    while let Some(field) = fields.next() {
        if field.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(field);
        match text.splitn(3, '\t').nth(2) {
            Some(path) if !path.is_empty() => paths.push(path.to_string()),
            _ => {
                for _ in 0..2 {
                    paths.extend(
                        fields
                            .next()
                            .map(|name| String::from_utf8_lossy(name).into_owned()),
                    );
                }
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_files_git_names_are_read_from_its_output() {
        assert_eq!(
            status_paths(b" M check.sh\0R  new.sh\0old.sh\0?? x y\0"),
            ["check.sh", "new.sh", "old.sh", "x y"]
        );
        assert_eq!(
            numstat_paths(b"1\t1\ta\x002\t0\t\0from\0to\0"),
            ["a", "from", "to"]
        );
    }

    #[test]
    fn a_merge_or_a_pick_changes_only_what_head_does_not_already_hold() {
        let words = |line: &str| line.split(' ').map(str::to_string).collect::<Vec<_>>();
        let Some(GitAsk::Diff(changes)) = merge_changes(&words("-m msg topic")) else {
            panic!("a merge is asked of git");
        };
        assert_eq!(changes, [[vec!["HEAD...topic"], vec!["HEAD", "topic"]]]);
        let Some(GitAsk::Diff(changes)) = pick_changes(&words("abc"), true) else {
            panic!("a revert is asked of git");
        };
        assert_eq!(changes, [[vec!["abc^1", "abc"], vec!["HEAD", "abc^1"]]]);
        assert!(merge_changes(&words("--abort")).is_none());
        assert!(clean_changes(&words("-n -fdx")).is_none());
        let Some(GitAsk::Clean(asked)) = clean_changes(&words("-fdx -e keep")) else {
            panic!("a clean is asked of git");
        };
        assert_eq!(asked, ["--dry-run", "-d", "-x", "-e", "keep", "--"]);
    }
}
