//! Every way of changing what proves the work that the guard has been
//! probed with (tests/guard_corpus.toml), judged by the real binary against
//! one project: what is refused stays refused, what must be allowed stays
//! allowed, and each known hole is said to be one until it is closed.
#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    row: Vec<Row>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    shell: String,
    command: String,
    protects: Vec<String>,
    status: String,
    note: String,
    /// The one system the row holds on, as `std::env::consts::OS` names it,
    /// for a path only that system has.
    #[serde(default)]
    os: Option<String>,
}

/// The flot binary under test, as tests/cli.rs finds it.
fn flot_bin() -> PathBuf {
    let declared = PathBuf::from(env!("CARGO_BIN_EXE_flot"));
    if declared.exists() {
        return declared;
    }
    let exe = std::env::current_exe().expect("the test binary has a path");
    exe.parent()
        .and_then(Path::parent)
        .expect("test binaries live in target/<profile>/deps")
        .join(declared.file_name().expect("the binary has a name"))
}

const FLOW: &str = r#"[flow]
name = "g"

[[step]]
id = "work"
run = "echo 1 > out.txt"
claims = ["out.txt holds 1"]
verify = "sh check.sh && sh tests/accept.sh"
verify_inputs = ["check.sh", "tests/accept.sh"]
"#;

const PATCH: &str = "--- a/check.sh\n+++ b/check.sh\n@@ -1 +1 @@\n-grep -q 1 out.txt\n+exit 0\n";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args([
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// The project the corpus describes, and a home directory for it.
fn fixture(dir: &Path, home: &Path) {
    write(&home.join(".claude/settings.json"), "{}\n");
    write(&home.join(".claude/plugins/p/hooks/hooks.json"), "{}\n");
    git(dir, home, &["init", "-q"]);
    write(&dir.join("flot.toml"), FLOW);
    write(&dir.join("check.sh"), "grep -q 0 out.txt\n");
    write(&dir.join("tests/accept.sh"), "grep -q 1 out.txt\n");
    write(&dir.join(".github/workflows/ci.yml"), "on: push\n");
    write(&dir.join(".claude/settings.json"), "{}\n");
    write(&dir.join("pkg/flot.toml"), "[flow]\nname = \"p\"\n");
    write(&dir.join("pkg/.claude/settings.json"), "{}\n");
    write(&dir.join(".gitignore"), ".flot/\nhard.sh\n");
    write(&dir.join("notes.md"), "notes\n");
    write(&dir.join("src.txt"), "0\n");
    std::os::unix::fs::symlink("check.sh", dir.join("alias.sh")).unwrap();
    std::os::unix::fs::symlink("tests", dir.join("t")).unwrap();
    git(dir, home, &["add", "-A"]);
    git(dir, home, &["commit", "-qm", "base"]);
    write(&dir.join("check.sh"), "grep -q 1 out.txt\n");
    git(dir, home, &["commit", "-qam", "work"]);
    git(dir, home, &["checkout", "-qb", "weak"]);
    write(&dir.join("check.sh"), "exit 0\n");
    git(dir, home, &["commit", "-qam", "weak"]);
    git(dir, home, &["checkout", "-q", "main"]);
    git(dir, home, &["checkout", "-qb", "other"]);
    write(&dir.join("notes.md"), "more notes\n");
    git(dir, home, &["commit", "-qam", "other"]);
    git(dir, home, &["checkout", "-q", "main"]);
    write(&dir.join("check.sh"), "grep -q 1 out.txt # changed\n");
    write(&dir.join(".flot/runs/r"), "{}\n");
    write(&dir.join("p.diff"), PATCH);
    std::fs::hard_link(dir.join("check.sh"), dir.join("hard.sh")).unwrap();
}

/// What `flot guard` made of one row: whether it refused, and the names
/// and reasons it gave.
struct Judged {
    refused: bool,
    said: Vec<(String, String)>,
    shown: String,
}

impl Judged {
    /// Whether the guard named `what`: as a path or refused command, or in
    /// a reason, such as "it leads to check.sh: ...". A hook row's
    /// denial holds it as written.
    fn names(&self, what: &str) -> bool {
        self.said.iter().any(|(path, reason)| {
            path == what
                || reason
                    .split([' ', ',', ';'])
                    .any(|word| word.trim_end_matches(':') == what)
                || (path.is_empty() && reason.contains(what))
        })
    }
}

fn judge(dir: &Path, home: &Path, row: &Row) -> Judged {
    let mut command = Command::new(flot_bin());
    command
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut stdin = String::new();
    match row.shell.as_str() {
        "sh" => {
            command.args(["guard", "--json", "--command", &row.command]);
        }
        "powershell" => {
            command.args(["guard", "--json", "--powershell", "--command", &row.command]);
        }
        "hook" => {
            command.args(["guard", "--hook"]);
            stdin = row
                .command
                .replace("{dir}", dir.to_str().unwrap())
                .replace("{home}", home.to_str().unwrap());
        }
        other => panic!("unknown shell {other:?} for {:?}", row.command),
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let shown = format!(
        "exit {:?}\n{stdout}{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    if row.shell == "hook" {
        assert_eq!(output.status.code(), Some(0), "the hook exits 0: {shown}");
        if stdout.trim().is_empty() {
            return Judged {
                refused: false,
                said: Vec::new(),
                shown,
            };
        }
        let denial: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let reason = denial["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .to_string();
        return Judged {
            refused: denial["hookSpecificOutput"]["permissionDecision"] == "deny",
            said: vec![(String::new(), reason)],
            shown,
        };
    }
    let found: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|_| panic!("flot guard --json printed JSON: {shown}"));
    let mut said = Vec::new();
    for key in ["protected", "refused"] {
        for item in found[key].as_array().unwrap() {
            said.push((
                item["path"].as_str().unwrap().to_string(),
                item["reason"].as_str().unwrap().to_string(),
            ));
        }
    }
    let refused = output.status.code() == Some(2);
    assert_eq!(
        refused,
        !said.is_empty(),
        "exit 2 exactly when something is refused: {shown}"
    );
    Judged {
        refused,
        said,
        shown,
    }
}

#[test]
fn the_guard_corpus_holds() {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/guard_corpus.toml"),
    )
    .unwrap();
    let mut corpus: Corpus = toml::from_str(&text).unwrap();
    corpus.row.retain(|row| {
        row.os
            .as_deref()
            .is_none_or(|os| os == std::env::consts::OS)
    });
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("project");
    let home = root.path().join("home");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    fixture(&dir, &home);

    // Each row only asks the guard; none changes the fixture, so the rows are
    // judged at once, as an agent's parallel tool calls would be.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let per_thread = corpus.row.len().div_ceil(threads).max(1);
    let judged: Vec<Judged> = std::thread::scope(|scope| {
        let workers: Vec<_> = corpus
            .row
            .chunks(per_thread)
            .map(|rows| {
                let (dir, home) = (&dir, &home);
                scope.spawn(move || {
                    rows.iter()
                        .map(|row| judge(dir, home, row))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect()
    });

    let mut wrong = Vec::new();
    for (row, judged) in corpus.row.iter().zip(judged) {
        assert!(
            !row.note.is_empty(),
            "{:?} says why it is here",
            row.command
        );
        let covered = row.protects.iter().all(|what| judged.names(what));
        let holds = match row.status.as_str() {
            "closed" => {
                assert!(
                    !row.protects.is_empty(),
                    "{:?} names what it changes",
                    row.command
                );
                judged.refused && covered
            }
            "allowed" => {
                assert!(
                    row.protects.is_empty(),
                    "{:?} protects nothing",
                    row.command
                );
                !judged.refused
            }
            "open" | "inherent" => {
                assert!(
                    !row.protects.is_empty(),
                    "{:?} names what it changes",
                    row.command
                );
                !(judged.refused && covered)
            }
            other => panic!("unknown status {other:?} for {:?}", row.command),
        };
        if !holds {
            wrong.push(format!(
                "[{}] {:?} is not {} (protects {:?}):\n{}",
                row.shell, row.command, row.status, row.protects, judged.shown
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} rows do not hold:\n\n{}",
        wrong.len(),
        corpus.row.len(),
        wrong.join("\n")
    );
}
