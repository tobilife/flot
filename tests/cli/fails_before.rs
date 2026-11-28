//! A proof that passes before the change proves nothing.

use crate::support::{commit, git_in, Project};

/// Proofs that must fail before the change: each step's proof passes on
/// the work, and `main` holds the code as it was.
const RED: &str = r#"
[flow]
name = "red"
proofs_from = "main"

[[step]]
id = "edited"
run = "true"
claims = ["app.txt is fixed"]
verify = "sh check.sh"
verify_inputs = ["check.sh"]
fails_before = true

[[step]]
id = "added"
run = "true"
claims = ["added.txt is there"]
verify = "test -f added.txt"
fails_before = true

[[step]]
id = "deleted"
run = "true"
claims = ["gone.txt is gone"]
verify = "test ! -f gone.txt"
fails_before = true

[[step]]
id = "same"
run = "true"
claims = ["check.sh is there"]
verify = "test -f check.sh"
fails_before = true

[[step]]
id = "built"
run = "echo built > out.bin"
outputs = ["out.bin"]
claims = ["out.bin is built"]
verify = "grep -q built out.bin"
fails_before = true

[[step]]
id = "local"
run = "true"
claims = ["local.env is set"]
verify = "test -f local.env"
fails_before = true
"#;

#[test]
fn a_proof_that_passes_before_the_change_proves_nothing() {
    let project = Project::new(RED);
    project.write("check.sh", "grep -q fixed app.txt\n");
    project.write("app.txt", "broken\n");
    project.write("gone.txt", "old\n");
    project.write(".gitignore", ".flot\n.test-keys/\nout.bin\nlocal.env\n");
    git_in(project.path(), &["init", "-q", "-b", "main"]);
    commit(project.path(), "base");
    // The change: an edit, a new file, a deletion. An ignored file the step
    // builds and one it does not are left as they are.
    project.write("app.txt", "fixed\n");
    project.write("added.txt", "new\n");
    std::fs::remove_file(project.path().join("gone.txt")).unwrap();
    project.write("local.env", "set\n");

    let run = project.flot(&["run"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    run.has("the proof passes with the changes undone to main (")
        .has("the proof exited 1 on a copy of the work, so failing there before the change would show nothing");
    let json = project.flot(&["run", "--json"]).json();
    let status = |step: &str| {
        json["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["step_id"] == step)
            .map(|entry| entry["claims"].clone())
            .unwrap()
    };
    for step in ["edited", "added", "deleted"] {
        assert_eq!(status(step)["verified"], 1, "{step}: {json}");
    }
    for step in ["same", "built", "local"] {
        assert_eq!(status(step)["unverified"], 1, "{step}: {json}");
    }
    let commit_id = git_in(project.path(), &["rev-parse", "main"]);
    project
        .flot(&["show", "latest"])
        .has(&format!(
            "before the change ({}): exited 1",
            &commit_id[..12]
        ))
        .has(&format!("before the change ({}): passed", &commit_id[..12]));
    // The work is untouched: the changes were undone in a copy.
    assert_eq!(
        std::fs::read_to_string(project.path().join("app.txt")).unwrap(),
        "fixed\n"
    );
    assert!(!project.path().join("gone.txt").exists());
    let verified = project.flot(&["verify"]);
    assert_eq!(verified.code, 2, "{}", verified.stdout);
    verified.has("proofs: as at main (");

    // Checked at one revision, a proof says nothing about the next.
    project.write("other.txt", "moved on\n");
    commit(project.path(), "next");
    project
        .flot(&["verify"])
        .has("its proof was run before the change at ")
        .has(", not at this revision");
    project
        .flot(&["run"])
        .has("cached receipt was not checked against this revision before the change");

    // Dropping the check is a change to the proof.
    project.write("flot.toml", &RED.replacen("fails_before = true\n", "", 1));
    project.flot(&["run"]).has("its fails_before differs");

    // Without a revision there is nothing to fail at, and what verified
    // before the step asked for the check does not stand in for it.
    let unchecked = RED
        .replace("proofs_from = \"main\"\n", "")
        .replace("fails_before = true\n", "");
    let plain = Project::new(&unchecked);
    plain.write("check.sh", "true\n");
    plain.flot(&["run", "--only", "edited"]).partial();
    plain.write("flot.toml", &RED.replace("proofs_from = \"main\"\n", ""));
    plain
        .flot(&["run", "--only", "edited"])
        .has("fails_before needs a revision to fail at: set proofs_from");

    // A file gets its mode at the revision back with its content.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tool = Project::new(
            "[flow]\nname = \"tool\"\nproofs_from = \"main\"\n\n[[step]]\nid = \"s\"\nrun = \"true\"\nclaims = [\"tool.sh says new\"]\nverify = 'test \"$(./tool.sh)\" != old'\nfails_before = true\n",
        );
        tool.write("tool.sh", "#!/bin/sh\necho old\n");
        tool.write(".gitignore", ".flot\n.test-keys/\n");
        let path = tool.path().join("tool.sh");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        git_in(tool.path(), &["init", "-q", "-b", "main"]);
        commit(tool.path(), "base");
        tool.write("tool.sh", "#!/bin/sh\necho new\n");
        tool.flot(&["run"]).ok();
    }

    // A check that cannot be made is refused where the flow is read.
    let refused = Project::new(
        "[flow]\nname = \"r\"\n\n[[step]]\nid = \"s\"\nrun = \"true\"\nfails_before = true\n",
    )
    .flot(&["plan"]);
    assert_eq!(refused.code, 3, "{}", refused.stderr);
    refused.has("step `s` sets fails_before, which needs claims and a verify command");
}
