//! A proof held to the tests the work did not change.

use crate::support::{commit, git_in, Project};

/// A step whose proof runs every test under tests/, held to the tests as
/// `main` has them.
const HELD: &str = r#"
[flow]
name = "held"
proofs_from = "main"

[[step]]
id = "s"
run = "true"
claims = ["app.txt is fixed"]
verify = 'for test in tests/*.sh; do [ -d "$test" ] || sh "$test" || exit 1; done'
held = ["tests/**"]
budget = { timeout_secs = 2 }
"#;

fn held_project() -> Project {
    let project = Project::new(HELD);
    project.write(
        "tests/check.sh",
        "grep -q fixed app.txt || { echo app.txt is not fixed; exit 1; }\n",
    );
    project.write("app.txt", "broken\n");
    project.write(".gitignore", ".flot\n.test-keys/\n");
    git_in(project.path(), &["init", "-q", "-b", "main"]);
    commit(project.path(), "base");
    project
}

#[test]
fn a_kept_copy_builds_again_what_the_work_changed() {
    // A build that goes by times, as cargo and make do, kept in the copies'
    // build from one run to the next.
    let project = held_project();
    let timed = HELD.replace("verify = 'for", "verify = 'sh build.sh && for");
    project.write("flot.toml", &timed);
    project.write(
        "build.sh",
        "out=\"${CARGO_TARGET_DIR:-build}/app\"; mkdir -p \"${out%/*}\"\n\
         [ \"$out\" -nt app.txt ] || cp app.txt \"$out\"\n",
    );
    project.write(
        "tests/check.sh",
        "grep -q fixed \"${CARGO_TARGET_DIR:-build}/app\" || { echo not fixed; exit 1; }\n",
    );
    project.write(".gitignore", ".flot\n.test-keys/\nbuild/\n");
    commit(project.path(), "build by times");

    // Fixed, with a test of its own that is as strict: verified.
    project.write("app.txt", "fixed\n");
    project.write(
        "tests/check.sh",
        "grep -q fixed \"${CARGO_TARGET_DIR:-build}/app\" || exit 1 # mine\n",
    );
    project.flot(&["run"]).ok().has("passed");

    // Broken again, with the test loosened: the copy must build the broken
    // file, not keep what it built from the fixed one.
    project.write("app.txt", "broken\n");
    project.write("tests/check.sh", "true\n");
    let run = project.flot(&["run", "--force"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    project.flot(&["log", "s", "--held"]).ok().has("not fixed");
}

#[test]
fn a_proof_that_passes_only_with_tests_the_work_changed_proves_nothing() {
    // A weakened test passes here and fails as the revision has it.
    let weakened = held_project();
    // A cargo build shared with the work would hand the work the copy's
    // binaries: the copy builds in a place of its own, without debug info
    // unless the step asks for it.
    let shared = HELD.replace(
        "budget =",
        "env = { CARGO_TARGET_DIR = \"/shared\", CARGO_BUILD_BUILD_DIR = \"/shared\", CARGO_INCREMENTAL = \"1\" }\nbudget =",
    );
    weakened.write("flot.toml", &shared);
    weakened.write(
        "tests/a-target.sh",
        "echo \"builds in $CARGO_TARGET_DIR and $CARGO_BUILD_BUILD_DIR\" | tr '\\\\' /\n\
         echo \"debug $CARGO_PROFILE_DEV_DEBUG $CARGO_PROFILE_TEST_DEBUG incremental $CARGO_INCREMENTAL\"\n",
    );
    commit(weakened.path(), "show where cargo builds");
    weakened.write("tests/check.sh", "true\n");
    // Each step's own build, as copies made them before: no proof uses it.
    weakened.write(".flot/builds/s/debug/flot", "built\n");
    let run = weakened.flot(&["run"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    assert!(!weakened.path().join(".flot/builds/s").exists());
    run.has("the proof exited 1 with tests/check.sh as main (")
        .has(") has it: the work passes only the versions it changed")
        .has("the work passes only the versions it changed; read `flot log s --held`");
    let held = weakened.flot(&["log", "s", "--held"]);
    // The store's path as the platform spells it: /private/var on macOS, a
    // short name on Windows.
    held.ok()
        .has("app.txt is not fixed")
        .has("/.flot/builds/target and ")
        .has("debug 0 0 incremental 1");
    assert!(
        held.stdout
            .lines()
            .any(|line| line.starts_with("builds in ") && line.ends_with("/.flot/builds/target")),
        "{}",
        held.stdout
    );
    weakened
        .flot(&["log", "s", "--proof"])
        .has("builds in /shared and /shared");
    weakened
        .flot(&["show", "latest"])
        .has("reason: the proof exited 1 with tests/check.sh as main (")
        .has("with held files as the revision has them: exited 1");
    // The work keeps the test it wrote.
    assert_eq!(
        std::fs::read_to_string(weakened.path().join("tests/check.sh")).unwrap(),
        "true\n"
    );

    // So does a deleted one, which the copy gets back.
    let deleted = held_project();
    std::fs::remove_file(deleted.path().join("tests/check.sh")).unwrap();
    deleted.write("tests/other.sh", "true\n");
    let run = deleted.flot(&["run"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    run.has("the proof exited 1 with tests/check.sh, tests/other.sh as main (")
        .has(") has them: the work passes");
    // Or one made a directory in its place.
    std::fs::remove_file(deleted.path().join("tests/other.sh")).unwrap();
    std::fs::create_dir(deleted.path().join("tests/check.sh")).unwrap();
    deleted.write("tests/check.sh/keep", "\n");
    let run = deleted.flot(&["run"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    run.has("the proof exited 1 with tests/check.sh, tests/check.sh/keep as main (");

    // A test that stops with the revision's slow test in place proves
    // nothing either.
    let slow = held_project();
    slow.write("tests/slow.sh", "sleep 5\n");
    commit(slow.path(), "slow");
    slow.write("app.txt", "fixed\n");
    slow.write("tests/slow.sh", "true\n");
    slow.flot(&["run"])
        .has("the proof timed out with tests/slow.sh as main (");

    // A fix passes the tests as they were, and a test it adds is no help.
    let fixed = held_project();
    fixed.write("app.txt", "fixed\n");
    fixed.write("tests/added.sh", "true\n");
    fixed.flot(&["run"]).ok();
    fixed
        .flot(&["show", "latest"])
        .has("with held files as the revision has them: passed");
    fixed.flot(&["log", "s", "--held"]).ok();
    fixed.flot(&["verify"]).ok();
    // Holding other files is a change to the proof.
    fixed.write("flot.toml", &HELD.replace("tests/**", "tests/*.sh"));
    fixed.flot(&["run"]).has("its held files differ");

    // With the tests as they were the proof already ran with them.
    let same = held_project();
    same.write("app.txt", "fixed\n");
    same.flot(&["run"]).ok();
    same.flot(&["show", "latest"])
        .has("held files: as the revision has them");
    let log = same.flot(&["log", "s", "--held"]);
    assert_eq!(log.code, 3, "{}", log.stderr);
    log.has("they are here as the revision has them");

    // A commit elsewhere leaves the verdict standing; one to the tests does
    // not.
    same.write("notes.txt", "later\n");
    commit(same.path(), "notes");
    same.flot(&["run"]).ok().has("cached");
    same.flot(&["verify"]).ok();
    same.write(
        "tests/check.sh",
        "grep -q fixed app.txt && grep -q more app.txt\n",
    );
    commit(same.path(), "stricter");
    same.write("app.txt", "fixed\n");
    same.flot(&["verify"])
        .has("its proof was held to other files than this revision has");
    let rerun = same.flot(&["run"]);
    rerun.has("cached receipt was not proven with the held files as this revision has them");
    assert_eq!(rerun.code, 1, "{}", rerun.stdout);

    // Without a revision there is nothing to hold the tests to.
    let loose = Project::new(&HELD.replace("proofs_from = \"main\"\n", ""));
    loose.write("tests/check.sh", "true\n");
    let run = loose.flot(&["run"]);
    run.partial()
        .has("held files need a revision to hold them to: set proofs_from");

    // A hold that cannot be made is refused where the flow is read.
    let refused = Project::new(
        "[flow]\nname = \"r\"\n\n[[step]]\nid = \"s\"\nrun = \"true\"\nheld = [\"tests/**\"]\n",
    )
    .flot(&["plan"]);
    assert_eq!(refused.code, 3, "{}", refused.stderr);
    refused.has("holds files to the revision, which needs claims and a verify command");
}
