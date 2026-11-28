//! Git in a test project: running it, committing, and a project committed on `main`.

use super::{Project, ANCHORED};
use std::path::Path;

/// Run git in `dir`, as a host with no git identity of its own.
pub(crate) fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", dir.join("no-global-gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A project committed on `main`, with `check.sh` as its proof.
pub(crate) fn anchored_project() -> Project {
    let project = Project::new(ANCHORED);
    project.write("check.sh", "grep -q 1 out.txt\n");
    project.write(".gitignore", ".flot\nout.txt\n");
    let dir = project.path();
    git_in(dir, &["init", "-q", "-b", "main"]);
    git_in(dir, &["add", "-A"]);
    git_in(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "base",
        ],
    );
    project
}

pub(crate) fn commit(dir: &Path, message: &str) {
    git_in(dir, &["add", "-A"]);
    git_in(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            message,
        ],
    );
}
