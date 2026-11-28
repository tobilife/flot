//! What `doctor` says weakens the evidence.

use crate::support::{anchored_project, flot_bin, git_in, Output, Project, CONFINED, PROVEN};
use std::process::Command;

#[test]
fn doctor_warns_of_a_proof_that_reads_no_declared_file() {
    let home = tempfile::tempdir().unwrap();
    let project = Project::new(&format!(
        "{PROVEN}\n[[step]]\nid = \"held\"\nrun = \"true\"\nclaims = [\"check.sh passes\"]\nverify = \"sh check.sh\"\nverify_inputs = [\"check.sh\"]\n\n[[step]]\nid = \"bare\"\nrun = \"true\"\nclaims = [\"nothing checks this\"]\n"
    ));
    project.write("check.sh", "true\n");
    let output = Command::new(flot_bin())
        .args(["doctor"])
        .current_dir(project.path())
        .env("HOME", home.path())
        .env("FLOT_SIGNING_KEY", project.key_path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a warning fails nothing: {stdout}"
    );
    assert!(
        stdout.contains("warn  proof inputs    proof of write reads no declared file, so an edit to it cannot be detected or guarded"),
        "{stdout}"
    );
    assert_eq!(stdout.matches("proof inputs").count(), 1, "{stdout}");
}

#[test]
fn doctor_names_every_proof_that_reads_no_declared_file_on_one_line() {
    let home = tempfile::tempdir().unwrap();
    let project = Project::new(&format!(
        "{PROVEN}\n[[step]]\nid = \"also\"\nrun = \"true\"\nclaims = [\"it holds\"]\nverify = \"true\"\n"
    ));
    let output = Command::new(flot_bin())
        .args(["doctor"])
        .current_dir(project.path())
        .env("HOME", home.path())
        .env("FLOT_SIGNING_KEY", project.key_path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("warn  proof inputs    proofs of write, also read no declared file, so an edit to them cannot be detected or guarded"),
        "{stdout}"
    );
    assert_eq!(stdout.matches("proof inputs").count(), 1, "{stdout}");
}

#[test]
fn doctor_names_what_weakens_the_evidence_and_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let doctor = |project: &Project| {
        let output = Command::new(flot_bin())
            .args(["doctor"])
            .current_dir(project.path())
            .env("HOME", home.path())
            .env("FLOT_SIGNING_KEY", project.key_path())
            .output()
            .unwrap();
        Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    };

    let loose = Project::new(PROVEN);
    git_in(loose.path(), &["init", "-q"]);
    doctor(&loose)
        .ok()
        .has("ok    flow            ")
        .has(": 1 step, 1 claim")
        .has("warn  proofs          proofs_from is not set")
        .has("warn  store           git does not ignore .flot/")
        .has("note  signing key     none")
        .has("note  trust           no store yet")
        .lacks("agent settings");
    assert!(!loose.path().join(".flot").exists(), "doctor made a store");

    let anchored = anchored_project();
    anchored.write(".gitignore", ".flot/\n");
    anchored.flot(&["key", "init"]).ok();
    anchored.flot(&["run"]).ok();
    doctor(&anchored)
        .ok()
        .has("ok    proofs          held to main (")
        .has("ok    store           .flot/ is ignored by git")
        .has("ok    signing key     ed25519:")
        .has("note  trust           no signers trusted")
        .has(
            "ok    history         no step or proof changed its result on the same inputs in 1 run",
        );

    // Settings that switch the guard off fail; others warn.
    let token = format!("sk-ant-{}", "x".repeat(24));
    anchored.write(
        ".claude/settings.local.json",
        &format!(
            r#"{{"disableAllHooks": true, "permissions": {{"defaultMode": "bypassPermissions"}}, "env": {{"ANTHROPIC_API_KEY": "{token}"}}}}"#
        ),
    );
    let failed = doctor(&anchored);
    assert_eq!(failed.code, 1, "{}", failed.stdout);
    failed
        .has("FAIL  agent settings  .claude/settings.local.json sets disableAllHooks")
        .has("warn  agent settings  .claude/settings.local.json runs every tool without asking")
        .has("warn  agent settings  .claude/settings.local.json holds what looks like a credential (Anthropic key)")
        .lacks(&token);
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::write(
        home.path().join(".claude/settings.json"),
        "{\"model\": \"x\"}",
    )
    .unwrap();
    std::fs::remove_file(anchored.path().join(".claude/settings.local.json")).unwrap();
    doctor(&anchored)
        .ok()
        .has("ok    agent settings  ~/.claude/settings.json leaves hooks on");

    // A sandbox is probed, not assumed.
    let confined = Project::new(CONFINED);
    let report = doctor(&confined);
    assert!(
        report
            .stdout
            .contains("ok    sandbox         inside, outside, offline: enforced by landlock")
            || report
                .stdout
                .contains("ok    sandbox         inside, outside, offline: enforced by seatbelt")
            || (report.code == 1
                && report.stdout.contains(
                    "FAIL  sandbox         inside, outside, offline: cannot be enforced here"
                )),
        "{}",
        report.stdout
    );
}
