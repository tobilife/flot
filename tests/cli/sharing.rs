//! Runs shared through a git remote.

use crate::support::{git_in, Project, PROVEN};

const LEAKY: &str = r#"
[flow]
name = "leaky"

[[step]]
id = "login"
run = "echo token=ghp_$(printf %036d 0)"
claims = ["it ran"]
verify = "true"
"#;

#[test]
fn a_run_that_carries_a_credential_does_not_leave_the_host() {
    let project = Project::new(LEAKY);
    git_in(project.path(), &["init", "-q"]);
    let remote = tempfile::tempdir().unwrap();
    git_in(remote.path(), &["init", "-q", "--bare"]);
    let url = remote.path().to_str().unwrap().to_string();
    project.flot(&["key", "init"]).ok();
    project.flot(&["run"]).ok();
    let token = format!("ghp_{}", "0".repeat(36));

    for share in [
        vec!["bundle", "--sign"],
        vec!["push", "--remote", &url, "--sign"],
    ] {
        let refused = project.flot(&share);
        assert_eq!(refused.code, 3, "{share:?}: {}", refused.stderr);
        refused
            .has("refusing to share run ")
            .has("nothing left this host")
            .has("  - step `login` stdout: GitHub token (ghp_…)")
            .has("--allow-secrets")
            .lacks(&token);
    }
    let bundles: Vec<_> = std::fs::read_dir(project.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(".flotbundle.json")
        })
        .collect();
    assert!(bundles.is_empty(), "a refused bundle was written");
    assert_eq!(git_in(remote.path(), &["for-each-ref"]), "");

    // The refusal came before signing: the run is still unsigned.
    project
        .flot(&["bundle", "--allow-secrets"])
        .ok()
        .has("unsigned");
    project
        .flot(&["push", "--remote", &url, "--allow-secrets"])
        .ok()
        .has("pushed run ");

    // A token the flow itself writes into a receipt, not only into output.
    let key_id = format!("AKIA{}", "Z".repeat(16));
    let quoted = Project::new(&format!(
        "[flow]\nname = \"quoted\"\n\n[[step]]\nid = \"cfg\"\nrun = \"true\"\nclaims = [\"deploys as {key_id}\"]\nverify = \"true\"\n"
    ));
    quoted.flot(&["run"]).ok();
    let refused = quoted.flot(&["bundle"]);
    assert_eq!(refused.code, 3, "{}", refused.stderr);
    refused
        .has("  - step `cfg` receipt: AWS access key (AKIA…)")
        .lacks(&key_id);
}

#[test]
fn runs_travel_through_a_git_remote_and_a_signed_one_stands_in() {
    let remote = tempfile::tempdir().unwrap();
    git_in(remote.path(), &["init", "-q", "--bare"]);
    let url = remote.path().to_str().unwrap().to_string();
    let alice = Project::new(PROVEN);
    let bob = Project::new(PROVEN);
    for project in [&alice, &bob] {
        git_in(project.path(), &["init", "-q"]);
    }

    // Nothing shared yet.
    bob.flot(&["pull", "--remote", &url])
        .ok()
        .has("has no shared runs (refs/flot/runs)");

    let key = alice.flot(&["key", "init", "--json"]).json()["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    alice.flot(&["run"]).ok();
    let run_id = alice.flot(&["show", "--json"]).json()["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice
        .flot(&["push", "--remote", &url, "--sign"])
        .ok()
        .has(&format!("pushed run {run_id} to "))
        .lacks("unsigned");
    alice
        .flot(&["push", "--remote", &url])
        .ok()
        .has(&format!("run {run_id} is already on "));
    // The ref holds the bundle; branches and the working tree are untouched.
    assert_eq!(
        git_in(
            remote.path(),
            &["ls-tree", "-r", "--name-only", "refs/flot/runs"]
        ),
        format!("proven/{run_id}.flotbundle.json")
    );
    assert_eq!(git_in(remote.path(), &["branch", "--list"]), "");

    bob.flot(&["trust", "add", "alice", &key]).ok();
    bob.flot(&["pull", "--remote", &url, "--require-trusted"])
        .ok()
        .has(&format!("imported run {run_id}  root "))
        .has("trusted  reusable")
        .has(": 1 imported, 0 already here, 0 refused");
    bob.flot(&["verify", &run_id]).ok().has("integrity: intact");
    // Alice's signed receipt stands in for Bob's work, on a tree that holds
    // what her step left.
    bob.write("artifact.txt", "flot\n");
    bob.flot(&["run"]).ok().has("cached   write");
    bob.flot(&["pull", "--remote", &url])
        .ok()
        .has(": 0 imported, 1 already here, 0 refused");

    // Bob's push builds on Alice's: both runs stay.
    bob.flot(&["run", "--force"]).ok();
    bob.flot(&["push", "--remote", &url]).ok().has("unsigned");
    let shared = git_in(
        remote.path(),
        &["ls-tree", "-r", "--name-only", "refs/flot/runs"],
    );
    assert_eq!(shared.lines().count(), 2, "{shared}");
    alice
        .flot(&["pull", "--remote", &url])
        .ok()
        .has("unsigned")
        .has(": 1 imported, 1 already here, 0 refused");
    let strict = alice.flot(&["pull", "--remote", &url, "--require-trusted"]);
    // Already imported, so nothing is refused the second time.
    strict.ok().has(": 0 imported, 2 already here, 0 refused");

    // A bundle filed under another run's name is refused, and named.
    let tip = git_in(remote.path(), &["rev-parse", "refs/flot/runs"]);
    let blob = git_in(
        remote.path(),
        &[
            "rev-parse",
            &format!("{tip}:proven/{run_id}.flotbundle.json"),
        ],
    );
    let tree = {
        let listing = format!("100644 blob {blob}\t1111111111111-aaaaaaaaaaaa.flotbundle.json\n");
        let mut child = std::process::Command::new("git")
            .args(["mktree"])
            .current_dir(remote.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(listing.as_bytes())
            .unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_string()
    };
    let outer = {
        let listing = format!("040000 tree {tree}\tproven\n");
        let mut child = std::process::Command::new("git")
            .args(["mktree"])
            .current_dir(remote.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(listing.as_bytes())
            .unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_string()
    };
    let forged = std::process::Command::new("git")
        .args(["commit-tree", &outer, "-m", "forged"])
        .current_dir(remote.path())
        .env("GIT_AUTHOR_NAME", "m")
        .env("GIT_AUTHOR_EMAIL", "m@example.com")
        .env("GIT_COMMITTER_NAME", "m")
        .env("GIT_COMMITTER_EMAIL", "m@example.com")
        .output()
        .unwrap();
    let forged = String::from_utf8(forged.stdout).unwrap().trim().to_string();
    git_in(remote.path(), &["update-ref", "refs/flot/runs", &forged]);
    let carol = Project::new(PROVEN);
    git_in(carol.path(), &["init", "-q"]);
    let refused = carol.flot(&["pull", "--remote", &url]);
    assert_eq!(refused.code, 1, "{}", refused.stdout);
    refused
        .has("refused  proven/1111111111111-aaaaaaaaaaaa.flotbundle.json: ")
        .has(&format!("carries run {run_id}"));
}

#[test]
fn sharing_needs_a_git_repository() {
    let project = Project::new(PROVEN);
    project.flot(&["run"]).ok();
    let outside = project.flot(&["push", "--remote", "origin"]);
    assert_eq!(outside.code, 4, "{}", outside.stderr);
    outside.has("sharing runs needs the flow to be in a git repository");
}
