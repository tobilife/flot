//! The file a path leads to, and every flow above it.

use super::{flot_from, hook_payload, two_proofs_project};
use crate::support::flot_bin;
use std::path::Path;
use std::process::Command;

#[test]
fn guard_judges_the_file_a_path_leads_to() {
    let project = two_proofs_project();
    let home = tempfile::tempdir().unwrap();
    let guard = |command: &str| {
        flot_from(
            &project,
            project.path(),
            home.path(),
            &["guard", "--command", command],
            "",
        )
    };
    for (command, said) in [
        ("rm \"$PWD/check.sh\"", "protected  check.sh"),
        ("rm ${PWD}/tests/accept.sh", "protected  tests/accept.sh"),
        (
            "echo '{}' > $HOME/.claude/settings.json",
            "protected  ~/.claude/settings.json",
        ),
        (
            "echo '{}' > ${HOME}/.claude/settings.local.json",
            "protected  ~/.claude/settings.local.json",
        ),
        (
            "rm ~/.claude/settings.json",
            "protected  ~/.claude/settings.json",
        ),
    ] {
        let guarded = guard(command);
        assert_eq!(guarded.code, 2, "{command}: {}", guarded.stdout);
        guarded.has(said);
    }
    #[cfg(unix)]
    {
        let dir = project.path();
        std::os::unix::fs::symlink("check.sh", dir.join("alias.sh")).unwrap();
        std::os::unix::fs::symlink("tests", dir.join("t")).unwrap();
        std::os::unix::fs::symlink(".claude/settings.local.json", dir.join("local.json")).unwrap();
        std::fs::hard_link(dir.join("check.sh"), dir.join("h.sh")).unwrap();
        for (command, said) in [
            (
                "echo 'exit 0' > alias.sh",
                "protected  alias.sh: it leads to check.sh: the proof of step `work` reads it",
            ),
            (
                "rm t/accept.sh",
                "protected  t/accept.sh: it leads to tests/accept.sh",
            ),
            (
                "echo '{}' > local.json",
                "protected  local.json: it leads to .claude/settings.local.json",
            ),
            (
                "echo 'exit 0' > h.sh",
                "protected  h.sh: it is a hard link to check.sh: the proof of step `work` reads it",
            ),
        ] {
            let guarded = guard(command);
            assert_eq!(guarded.code, 2, "{command}: {}", guarded.stdout);
            guarded.has(said);
        }
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        let guarded = project.flot(&["guard", "FLOT.TOML", "Check.SH"]);
        assert_eq!(guarded.code, 2, "{}", guarded.stdout);
        guarded
            .has("protected  FLOT.TOML: it is the flow file")
            .has("protected  Check.SH: the proof of step `work` reads it");
    }
}

/// Ask the flot binary at `bin` whether `bin` is protected, from `dir`.
fn guard_own_binary(bin: &Path, dir: &Path, home: &Path) {
    // Another test's child may still hold the copy open for writing for a
    // moment after it was written (ETXTBSY on Linux).
    let own = (0..100)
        .find_map(|_| {
            match Command::new(bin)
                .args(["guard", bin.to_str().unwrap()])
                .current_dir(dir)
                .env("HOME", home)
                .env("USERPROFILE", home)
                .output()
            {
                Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    None
                }
                other => Some(other.unwrap()),
            }
        })
        .expect("the copied binary starts");
    let said = String::from_utf8_lossy(&own.stdout);
    assert_eq!(own.status.code(), Some(2), "{said}");
    assert!(
        said.contains("it is the flot binary that enforces this guard"),
        "{said}"
    );
}

#[test]
fn guard_consults_every_flow_above_a_file() {
    let project = two_proofs_project();
    let home = tempfile::tempdir().unwrap();
    let flot_in =
        |dir: &Path, args: &[&str], stdin: &str| flot_from(&project, dir, home.path(), args, stdin);
    // Another flow file inside the project cannot be created...
    let decoy = project.flot(&["guard", "tests/flot.toml"]);
    assert_eq!(decoy.code, 2, "{}", decoy.stdout);
    decoy.has("protected  tests/flot.toml: it would be another flow file");
    // ...and one that is there does not hide the flow above it.
    project.write("tests/flot.toml", "[flow]\nname = \"decoy\"\n");
    let tests = project.path().join("tests");
    let edit = hook_payload(
        &tests,
        "Edit",
        serde_json::json!({ "file_path": tests.join("accept.sh") }),
    );
    flot_in(&tests, &["guard", "--hook"], &edit)
        .ok()
        .has("\"permissionDecision\": \"deny\"")
        .has("tests/accept.sh is protected: the proof of step `work` reads it");
    let from_tests = flot_in(&tests, &["guard", "accept.sh"], "");
    assert_eq!(from_tests.code, 2, "{}", from_tests.stdout);

    // Claude Code settings anywhere in the repository, as a subproject
    // with its own flow names them.
    project.write(
        "pkg/flot.toml",
        "[flow]\nname = \"p\"\n\n[[step]]\nid = \"s\"\nrun = \"true\"\n",
    );
    let settings = flot_in(
        &project.path().join("pkg"),
        &["guard", ".claude/settings.json"],
        "",
    );
    assert_eq!(settings.code, 2, "{}", settings.stdout);
    settings.has("protected  pkg/.claude/settings.json: it configures Claude Code");
    let nested = project.flot(&["guard", "docs/.claude/settings.local.json"]);
    assert_eq!(nested.code, 2, "{}", nested.stdout);

    // The user's own Claude Code settings and plugins, and the flot that
    // runs the guard.
    let plugin = home.path().join(".claude/plugins/flot/hooks/hooks.json");
    std::fs::create_dir_all(plugin.parent().unwrap()).unwrap();
    std::fs::write(&plugin, "{}").unwrap();
    for (path, said) in [
        (
            home.path().join(".claude/settings.local.json"),
            "protected  ~/.claude/settings.local.json: it configures Claude Code for this user",
        ),
        (
            plugin.clone(),
            "protected  ~/.claude/plugins/flot/hooks/hooks.json: it is a Claude Code plugin",
        ),
        (flot_bin(), "it is the flot binary that enforces this guard"),
    ] {
        let guarded = flot_in(project.path(), &["guard", path.to_str().unwrap()], "");
        assert_eq!(guarded.code, 2, "{}", guarded.stdout);
        guarded.has(said);
    }
    // Wherever the binary is: a proof built on a copy of the work leaves
    // it in the store.
    let built = project
        .path()
        .join(".flot/builds/s/debug")
        .join(flot_bin().file_name().unwrap());
    std::fs::create_dir_all(built.parent().unwrap()).unwrap();
    std::fs::copy(flot_bin(), &built).unwrap();
    guard_own_binary(&built, project.path(), home.path());
    // Also when the project is named through a link and the binary's own
    // path is resolved, as macOS's /var is /private/var.
    #[cfg(unix)]
    {
        let alias = home.path().join("alias");
        std::os::unix::fs::symlink(project.path(), &alias).unwrap();
        let named = alias
            .join(".flot/builds/s/debug")
            .join(flot_bin().file_name().unwrap());
        guard_own_binary(&named, &alias, home.path());
    }
    let tree = flot_in(
        project.path(),
        &["guard", "--command", "rm -rf ~/.claude"],
        "",
    );
    assert_eq!(tree.code, 2, "{}", tree.stdout);
    tree.has("protected  ~/.claude: it holds ~/.claude/plugins");
}

/// A key named through a link to the project (macOS's `/var` is one to
/// `/private/var`) is named from the project all the same.
#[cfg(unix)]
#[test]
fn guard_names_a_secret_reached_through_a_link_from_the_project() {
    let project = two_proofs_project();
    project.flot(&["key", "init"]).ok();
    project.flot(&["plan"]);
    let link = tempfile::tempdir().unwrap();
    let through = link.path().join("project");
    std::os::unix::fs::symlink(project.path(), &through).unwrap();
    let key = through.join(".test-keys/signing.key");
    project
        .flot(&[
            "guard",
            "--command",
            &format!("cp {} /tmp/k", key.display()),
        ])
        .has("secret     .test-keys/signing.key: ");
}
