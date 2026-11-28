//! The signing key and the flot commands that change whose evidence counts.

use super::{flot_from, hook_payload, two_proofs_project};
use crate::support::flot_bin;
use std::path::Path;
use std::process::Command;

#[test]
fn guard_refuses_whatever_reads_the_signing_key_flow_or_not() {
    let project = two_proofs_project();
    project.flot(&["key", "init"]).ok();
    // A run opens the store, which makes the host secret beside the key.
    project.flot(&["plan"]);
    let keys = project.path().join(".test-keys");
    assert!(keys.join("host.secret").exists());
    std::fs::write(keys.join("signing.key.retired-0123456789abcdef"), "old").unwrap();
    std::fs::write(keys.join("signing.pub"), "ed25519:x\n").unwrap();
    let key = project.key_path();
    let key = key.to_str().unwrap();
    for (command, named) in [
        (
            "cat .test-keys/signing.key".to_string(),
            ".test-keys/signing.key",
        ),
        (
            "base64 < $FLOT_SIGNING_KEY".to_string(),
            ".test-keys/signing.key",
        ),
        // Quoted, as a Windows path must be for sh to keep its backslashes.
        (format!("cp \"{key}\" /tmp/k"), ".test-keys/signing.key"),
        (
            "cp .test-keys/host.secret /tmp/s".to_string(),
            ".test-keys/host.secret",
        ),
        (
            "cat .test-keys/signing.key.retired-*".to_string(),
            ".test-keys/signing.key.retired-0123456789abcdef",
        ),
        ("tar czf /tmp/k.tgz .test-keys".to_string(), ".test-keys"),
        (
            "cd .test-keys && xxd signing.key".to_string(),
            ".test-keys/signing.key",
        ),
        (
            "python3 -c \"print(open('.test-keys/host.secret').read())\"".to_string(),
            ".test-keys/host.secret",
        ),
    ] {
        project
            .flot(&["guard", "--command", &command])
            .has(&format!("secret     {named}: "))
            .has("an agent that reads it can");
    }
    for allowed in [
        "flot key show",
        "cat .test-keys/signing.pub",
        "cat check.sh",
    ] {
        project.flot(&["guard", "--command", allowed]).ok();
    }
    let read = |path: &Path| {
        hook_payload(
            project.path(),
            "Read",
            serde_json::json!({ "file_path": path }),
        )
    };
    project
        .flot_stdin(&["guard", "--hook"], &read(&project.key_path()))
        .ok()
        .has("\"permissionDecision\": \"deny\"")
        .has("must not be read: it is the signing key")
        .has("`flot key show` prints the public key");
    let allowed = project.flot_stdin(&["guard", "--hook"], &read(&keys.join("signing.pub")));
    assert_eq!(allowed.ok().stdout.trim(), "");
    // Where no flow is, the key is refused all the same.
    let elsewhere = tempfile::tempdir().unwrap();
    let outside = flot_from(
        &project,
        elsewhere.path(),
        elsewhere.path(),
        &["guard", "--command", &format!("cat \"{key}\"")],
        "",
    );
    assert_eq!(outside.code, 2, "{}", outside.stdout);
    outside.has("it is the signing key");
    let payload = hook_payload(
        elsewhere.path(),
        "Read",
        serde_json::json!({ "file_path": project.key_path() }),
    );
    flot_from(
        &project,
        elsewhere.path(),
        elsewhere.path(),
        &["guard", "--hook"],
        &payload,
    )
    .ok()
    .has("\"permissionDecision\": \"deny\"");
    flot_from(
        &project,
        elsewhere.path(),
        elsewhere.path(),
        &["guard", "--command", "rm -rf x"],
        "",
    )
    .ok();
    // A key kept in the project leaves the project's directory to the work.
    let beside = |command: &str| {
        Command::new(flot_bin())
            .args(["guard", "--command", command])
            .current_dir(project.path())
            .env("FLOT_SIGNING_KEY", project.path().join("signing.key"))
            .output()
            .unwrap()
    };
    assert_eq!(beside("ls . && grep -r x .").status.code(), Some(0));
    assert_eq!(beside("cat signing.key").status.code(), Some(2));
}

#[test]
fn guard_refuses_the_flot_commands_that_change_whose_evidence_counts() {
    let project = two_proofs_project();
    for (command, shape) in [
        ("flot init --force", "flot init --force"),
        ("flot trust add me ed25519:AAAA", "flot trust add"),
        ("flot trust remove me", "flot trust remove"),
        ("flot trust rotate handover.json", "flot trust rotate"),
        ("flot key init", "flot key init"),
        ("flot key rotate", "flot key rotate"),
        ("flot sign", "flot sign"),
        ("flot import run.flotbundle.json", "flot import"),
        ("flot pull --remote origin", "flot pull"),
        ("flot gc", "flot gc"),
        ("flot run --proofs-from HEAD", "flot run --proofs-from"),
        (
            "flot verify --proofs-from=HEAD latest",
            "flot verify --proofs-from",
        ),
        ("flot -f other.toml run", "flot run -f"),
        ("flot diff --file other.toml", "flot diff -f"),
        ("cargo build && ./target/debug/flot gc", "flot gc"),
        ("sudo -u root flot sign", "flot sign"),
        ("FLOT_SIGNING_KEY=k timeout 5 flot pull", "flot pull"),
        ("bash -c 'flot key rotate'", "flot key rotate"),
    ] {
        let refused = project.flot(&["guard", "--command", command]);
        assert_eq!(refused.code, 2, "{command}: {}", refused.stdout);
        refused.has(&format!(
            "refused    {shape}: it changes whose evidence counts; ask the user to run it"
        ));
    }
    for command in [
        "flot run",
        "flot run --only work -j 2",
        "flot plan --proofs-from HEAD",
        "flot --json show",
        "flot verify",
        "flot log work",
        "flot runs",
        "flot search x",
        "flot flaky",
        "flot doctor",
        "flot guard check.sh",
        "flot trust list",
        "flot key show",
        "flot gc --dry-run",
        "echo flot gc",
        "git commit -m 'flot gc'",
    ] {
        let allowed = project.flot(&["guard", "--command", command]);
        assert_eq!(allowed.code, 0, "{command}: {}", allowed.stdout);
    }
    for command in [
        "flot.exe gc",
        "& flot trust add me k",
        "C:\\tools\\flot.exe sign",
    ] {
        let refused = project.flot(&["guard", "--powershell", "--command", command]);
        assert_eq!(refused.code, 2, "{command}: {}", refused.stdout);
    }
    project
        .flot_stdin(
            &["guard", "--hook"],
            &hook_payload(
                project.path(),
                "Bash",
                serde_json::json!({ "command": "flot init --force" }),
            ),
        )
        .ok()
        .has("\"permissionDecision\": \"deny\"")
        .has("`flot init --force` changes whose evidence counts; ask the user to run it");
}
