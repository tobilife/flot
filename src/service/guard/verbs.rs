//! The `flot` commands a command line runs that an agent should leave to
//! the user, found among the commands the readers resolved.

use crate::core::shell::Command;

/// The program a word runs, by its file name, in lowercase: a system that
/// does not tell case apart runs `FLOT` as `flot`.
pub(super) fn program_name(word: &str) -> String {
    word.rsplit(['/', '\\'])
        .next()
        .unwrap_or(word)
        .to_lowercase()
}

/// Each way `command` may run, its program first: as resolved, or from
/// each word on when its program may be any of them.
pub(super) fn runs(command: &Command) -> impl Iterator<Item = &[String]> {
    let starts = if command.unresolved {
        command.words.len()
    } else {
        command.words.len().min(1)
    };
    (0..starts).map(|at| &command.words[at..])
}

/// The `flot` command a resolved command of `words`, its program first,
/// runs when an agent should leave it to the user, as spelled by its verb:
/// `flot init --force`, `flot trust add`, ... A flot named by its path
/// counts; `echo flot gc` does not.
pub(super) fn flot_verb(words: &[String]) -> Option<String> {
    let (program, args) = words.split_first()?;
    if !matches!(program_name(program).as_str(), "flot" | "flot.exe") {
        return None;
    }
    refused_verb(args)
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
    use crate::core::{powershell, shell};

    fn verbs(commands: &[shell::Command]) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        for verb in commands.iter().flat_map(runs).filter_map(flot_verb) {
            if !found.contains(&verb) {
                found.push(verb);
            }
        }
        found
    }

    #[test]
    fn a_flot_command_is_found_where_it_runs_and_not_where_it_is_text() {
        let posix = |command: &str| verbs(&shell::read(command).commands);
        for (command, verb) in [
            ("flot gc", "flot gc"),
            ("timeout 5 flot gc", "flot gc"),
            ("FOO=1 ./target/debug/flot sign r", "flot sign"),
            ("bash -c 'flot trust add a b'", "flot trust add"),
            ("true && flot -f other.toml run", "flot run -f"),
            ("flot run --proofs-from=main", "flot run --proofs-from"),
            ("flot init --force", "flot init --force"),
            ("FLOT gc", "flot gc"),
            ("SUDO -u root flot gc", "flot gc"),
            ("ash -c 'flot gc'", "flot gc"),
            ("mksh -c 'flot trust add ci AAAA'", "flot trust add"),
            ("busybox ash -c 'flot gc'", "flot gc"),
            ("sh -c \"bash -lc 'flot gc'\"", "flot gc"),
            ("chronic flot gc", "flot gc"),
            ("strace -f -o /tmp/log -e trace=file flot gc", "flot gc"),
            ("ltrace -l libc.so.6 flot gc", "flot gc"),
            ("unbuffer -p flot gc", "flot gc"),
            ("eval flot gc", "flot gc"),
            ("echo x | xargs flot gc", "flot gc"),
            ("find . -name x -exec flot gc \\;", "flot gc"),
            ("echo \"$(flot gc)\"", "flot gc"),
            ("sh <<'EOF'\nflot gc\nEOF", "flot gc"),
            ("cmd //c flot gc", "flot gc"),
            ("pwsh -c 'flot gc'", "flot gc"),
            ("powershell.exe -Command \"flot gc\"", "flot gc"),
            (
                "eval eval eval eval eval eval eval eval eval eval flot gc",
                "flot gc",
            ),
            // A program a wrapper runs that the reader does not know may
            // be a wrapper, and one it feeds text may be a shell.
            ("sudo foo flot gc", "flot gc"),
            ("foo <<'EOF'\nflot gc\nEOF", "flot gc"),
            ("sudo -s <<'EOF'\nflot gc\nEOF", "flot gc"),
            ("su <<'EOF'\nflot gc\nEOF", "flot gc"),
            ("ssh host <<'EOF'\nflot gc\nEOF", "flot gc"),
            ("ssh -p 22 host flot gc", "flot gc"),
            ("F=flot; $F gc", "flot gc"),
        ] {
            assert_eq!(posix(command), [verb], "{command}");
        }
        for command in [
            "echo flot gc",
            "git commit -m 'flot gc'",
            "flot gc --dry-run",
            "flot verify",
            "flot trust list",
            "flot init",
            "cat <<'EOF'\nflot gc\nEOF",
            "command -v flot gc",
            "# flot gc",
            "sudo apt install flot",
            "timeout 5 echo flot gc",
            "sudo echo flot gc",
            "git commit -F- <<'EOF'\nflot gc\nEOF",
            "iex 'flot gc'",
            "foo flot gc",
        ] {
            assert!(posix(command).is_empty(), "{command}");
        }
        let powershell = |command: &str| verbs(&powershell::read(command).commands);
        for (command, verb) in [
            ("flot.exe gc", "flot gc"),
            ("& flot trust add me k", "flot trust add"),
            ("C:\\tools\\flot.exe sign", "flot sign"),
            ("iex 'flot gc'", "flot gc"),
            ("Invoke-Expression \"flot key rotate\"", "flot key rotate"),
            ("cmd /c \"flot gc\"", "flot gc"),
            ("Start-Process flot -ArgumentList 'gc'", "flot gc"),
            ("$x = flot sign", "flot sign"),
        ] {
            assert_eq!(powershell(command), [verb], "{command}");
        }
        for command in [
            "Write-Output 'flot gc'",
            "echo flot gc",
            "flot gc -WhatIf --dry-run",
        ] {
            assert!(powershell(command).is_empty(), "{command}");
        }
    }
}
