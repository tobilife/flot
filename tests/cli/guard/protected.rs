//! What proves the work, and what decides whether it is checked.

use super::two_proofs_project;
use crate::support::{anchored_project, commit, flot_bin, git_in, Project, ANCHORED};
use std::process::Command;

#[test]
fn guard_protects_what_decides_whether_proofs_are_checked() {
    let project = Project::new(ANCHORED);
    git_in(project.path(), &["init", "-q"]);
    let harness = [
        (".claude/settings.json", "it configures Claude Code"),
        (".claude/settings.local.json", "it configures Claude Code"),
        (".github/workflows/ci.yml", "it is a CI workflow"),
        (".git/hooks/pre-commit", "it is a git hook"),
        (".git/config", "it is git configuration"),
    ];
    for (path, reason) in harness {
        let guarded = project.flot(&["guard", path]);
        assert_eq!(guarded.code, 2, "{path}: {}", guarded.stdout);
        guarded.has(&format!("protected  {path}: {reason}"));
    }
    // Outside a repository, the flow's directory stands in for it.
    let bare = Project::new(ANCHORED);
    bare.flot(&["guard", ".claude/settings.json"])
        .has("protected  .claude/settings.json: it configures Claude Code");
    for path in [
        ".github/dependabot.yml",
        ".claude/agents/reviewer.md",
        ".git-blame-ignore-revs",
        "docs/.github/workflows/ci.yml",
    ] {
        project.flot(&["guard", path]).ok();
    }

    // A flow in a subdirectory guards its repository's harness, named from
    // the repository, and once however many flows find it.
    project.write(
        "sub/flot.toml",
        "[flow]\nname = \"sub\"\n\n[[step]]\nid = \"inner\"\nrun = \"true\"\n",
    );
    let nested = project.flot(&["-f", "sub/flot.toml", "guard", ".github/workflows/ci.yml"]);
    assert_eq!(nested.code, 2, "{}", nested.stdout);
    nested.has("protected  .github/workflows/ci.yml: it is a CI workflow");
    let both = project.flot(&["guard", "sub/x", ".github/workflows/ci.yml"]);
    assert_eq!(both.stdout.matches("ci.yml").count(), 1, "{}", both.stdout);

    // The user's own Claude Code settings, wherever the session is.
    let home = tempfile::tempdir().unwrap();
    let settings = home.path().join(".claude/settings.json");
    let output = Command::new(flot_bin())
        .args(["guard", settings.to_str().unwrap()])
        .current_dir(project.path())
        .env("HOME", home.path())
        .env("FLOT_SIGNING_KEY", project.key_path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(2), "{stdout}");
    assert!(
        stdout.contains(
            "protected  ~/.claude/settings.json: it configures Claude Code for this user"
        ),
        "{stdout}"
    );
    // Named with `~` in a shell command, as the shell would expand it.
    let output = Command::new(flot_bin())
        .args(["guard", "--command", "rm ~/.claude/settings.json"])
        .current_dir(project.path())
        .env("HOME", home.path())
        .env("FLOT_SIGNING_KEY", project.key_path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("protected  ~/.claude/settings.json"),
        "{stdout}"
    );
}

#[test]
fn guard_names_the_files_that_prove_the_work() {
    let project = Project::new(ANCHORED);
    project.write("check.sh", "true\n");
    let guarded = project.flot(&[
        "guard",
        "check.sh",
        "flot.toml",
        ".flot/latest",
        "src/lib.rs",
    ]);
    assert_eq!(guarded.code, 2, "{}", guarded.stdout);
    guarded
        .has("protected  check.sh: the proof of step `work` reads it")
        .has("protected  flot.toml: it is the flow file")
        .has("protected  .flot/latest: it is in the flot store")
        .lacks("src/lib.rs");
    project.flot(&["guard", "src/lib.rs"]).ok();
    // A file of a subproject is guarded by the subproject's flow.
    project.write(
        "sub/flot.toml",
        "[flow]\nname = \"sub\"\n\n[[step]]\nid = \"inner\"\nrun = \"true\"\nclaims = [\"t\"]\nverify = \"sh t.sh\"\nverify_inputs = [\"t.sh\"]\n",
    );
    let nested = project.flot(&["guard", "sub/t.sh"]);
    assert_eq!(nested.code, 2, "{}", nested.stdout);
    nested.has("protected  t.sh: the proof of step `inner` reads it");
    // So is the allow list of a step's mutants, there or not.
    let mutants = project.flot(&["guard", "sub/allow.toml"]);
    assert_eq!(mutants.code, 0, "{}", mutants.stdout);
    project.write(
        "sub/flot.toml",
        "[flow]\nname = \"sub\"\n\n[[step]]\nid = \"inner\"\nrun = \"true\"\nclaims = [\"t\"]\nverify = \"sh t.sh\"\nmutants = { run = \"m\", report = \"r\", allow = \"allow.toml\" }\n",
    );
    let mutants = project.flot(&["guard", "sub/allow.toml"]);
    assert_eq!(mutants.code, 2, "{}", mutants.stdout);
    mutants.has("protected  allow.toml: the proof of step `inner` reads it");

    // As a Claude Code hook: the payload names the file and the session's
    // directory, and a protected file is denied on stdout with exit 0.
    let payload = |path: &str| {
        serde_json::json!({
            "cwd": project.path(),
            "tool_name": "Edit",
            "tool_input": { "file_path": path },
        })
        .to_string()
    };
    let denied = project.flot_stdin(&["guard", "--hook"], &payload("check.sh"));
    denied
        .ok()
        .has("\"permissionDecision\": \"deny\"")
        .has("check.sh is protected");
    let absolute = project.path().join("src/lib.rs");
    let allowed = project.flot_stdin(&["guard", "--hook"], &payload(absolute.to_str().unwrap()));
    assert_eq!(allowed.ok().stdout.trim(), "");
}

#[test]
fn guard_refuses_a_directory_that_holds_what_proves_the_work() {
    let project = two_proofs_project();
    project.write("build/out.o", "x");
    for (command, named) in [
        ("rm -rf tests", "tests/accept.sh"),
        ("rm -rf .github", ".github/workflows"),
        ("rm -rf .git/hooks", ".git/hooks"),
        ("rm -rf .git", ".git/config"),
        ("rm -rf .claude", ".claude/settings.json"),
        ("rm -rf .", "flot.toml"),
        ("cp -r /tmp/fake/tests .", "tests/accept.sh"),
        ("git checkout HEAD~1 -- tests", "tests/accept.sh"),
    ] {
        let guarded = project.flot(&["guard", "--command", command]);
        assert_eq!(guarded.code, 2, "{command}: {}", guarded.stdout);
        guarded.has(named);
    }
    project
        .flot(&["guard", "tests"])
        .has("protected  tests: it holds tests/accept.sh: the proof of step `work` reads it");
    // A new file in such a directory is only that file.
    project.flot(&["guard", "tests/new.sh"]).ok();
    project
        .flot(&["guard", "--command", "cp /tmp/x tests/new.sh"])
        .ok();
    project.flot(&["guard", "--command", "rm -rf build"]).ok();
}

#[test]
fn a_whole_tree_git_command_is_refused_when_it_would_change_a_proof() {
    let project = anchored_project();
    let dir = project.path();
    // A staged change that is not a proof.
    project.write("notes.md", "draft\n");
    git_in(dir, &["add", "notes.md"]);
    for command in [
        "git stash",
        "git stash -u",
        "git reset --hard",
        "git checkout -f",
        "git checkout main",
    ] {
        project.flot(&["guard", "--command", command]).ok();
    }
    // The proof itself is changed: each of these would put it back.
    project.write("check.sh", "true\n");
    for command in [
        "git stash",
        "git stash push -u",
        "git reset --hard",
        "git reset --hard HEAD",
        "git checkout -f",
        "cargo test && git -C . stash",
    ] {
        let guarded = project.flot(&["guard", "--command", command]);
        assert_eq!(guarded.code, 2, "{command}: {}", guarded.stdout);
        guarded.has("protected  check.sh: the proof of step `work` reads it; `git ");
    }
    // Switching keeps the change, so only what differs at the revision counts.
    project
        .flot(&["guard", "--command", "git checkout main"])
        .ok();
    git_in(dir, &["checkout", "-q", "check.sh"]);
    git_in(dir, &["checkout", "-q", "-b", "weak"]);
    project.write("check.sh", "true\n");
    commit(dir, "weaken");
    git_in(dir, &["checkout", "-q", "main"]);
    for command in [
        "git checkout weak",
        "git switch weak",
        "git reset --hard weak",
    ] {
        let guarded = project.flot(&["guard", "--command", command]);
        assert_eq!(guarded.code, 2, "{command}: {}", guarded.stdout);
        guarded.has("protected  check.sh");
    }
    // When git cannot say, the command is refused.
    let unsure = project.flot(&["guard", "--command", "git reset --hard nowhere"]);
    assert_eq!(unsure.code, 2, "{}", unsure.stdout);
    unsure.has("refused    git reset --hard nowhere: git cannot say which files it would change");
    // The rest of the line is still read.
    let both = project.flot(&["guard", "--command", "git stash; rm -rf ."]);
    assert_eq!(both.code, 2, "{}", both.stdout);
    both.has("protected  .: it holds flot.toml");
}
