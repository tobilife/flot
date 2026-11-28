//! Proofs held to a revision, and the changes no step read.

use crate::support::{
    anchored_project, commit, flot_bin, git_in, read, run_id, Project, ANCHORED, TWO_STEPS,
};
use std::process::Command;

#[test]
fn verify_names_the_changes_no_step_read() {
    let project = anchored_project();
    project.write("docs/old.md", "old\n");
    git_in(project.path(), &["add", "-A"]);
    git_in(
        project.path(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "docs",
        ],
    );
    // The step now reads src/; the flow file itself is not a change here.
    project.write(
        "flot.toml",
        &ANCHORED.replace(
            "run = \"echo 1 > out.txt\"",
            "run = \"echo 1 > out.txt\"\ninputs = [\"src/*.txt\"]",
        ),
    );
    project.write("src/read.txt", "read\n");
    project.write("src/unread.md", "not an input\n");
    project.write("notes/a.md", "untracked\n");
    project.write("notes/b.md", "untracked\n");
    project.write(".gitignore", ".flot\nout.txt\nscratch/\n.test-keys/\n");
    project.write("scratch/tmp.txt", "ignored\n");
    std::fs::remove_file(project.path().join("docs/old.md")).unwrap();
    project.flot(&["run"]).ok();
    let verified = project.flot(&["verify"]);
    verified
        .ok()
        .has("proofs: as at main (")
        .has("changes no step read: .gitignore, docs/old.md, notes/, src/unread.md")
        .lacks("src/read.txt")
        .lacks("scratch")
        .lacks("flot.toml");
    let json = project.flot(&["verify", "--json"]).json();
    assert_eq!(
        json["anchor"]["unread"],
        serde_json::json!([".gitignore", "docs/old.md", "notes/", "src/unread.md"])
    );
    project
        .flot(&["verify", "--markdown"])
        .ok()
        .has(r"- Changes no step read: \.gitignore, docs/old\.md, notes/, src/unread\.md");

    // A changed proof file is the proof's business, not an unread change.
    project.write("check.sh", "grep -q 1 out.txt # changed\n");
    project.flot(&["run"]);
    let drifted = project.flot(&["verify"]);
    assert_eq!(drifted.code, 2, "{}", drifted.stdout);
    drifted
        .has("check.sh differs")
        .has("changes no step read: .gitignore, docs/old.md, notes/, src/unread.md\n");

    // A flow in a subdirectory sees only its own directory, named from it.
    let repo = tempfile::tempdir().unwrap();
    let app = repo.path().join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("flot.toml"), ANCHORED).unwrap();
    std::fs::write(app.join("check.sh"), "grep -q 1 out.txt\n").unwrap();
    std::fs::write(
        repo.path().join(".gitignore"),
        ".flot\nout.txt\n.test-keys/\n",
    )
    .unwrap();
    std::fs::write(app.join("data.txt"), "old\n").unwrap();
    std::fs::write(repo.path().join("top.txt"), "old\n").unwrap();
    git_in(repo.path(), &["init", "-q", "-b", "main"]);
    git_in(repo.path(), &["add", "-A"]);
    git_in(
        repo.path(),
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
    std::fs::write(app.join("lib.txt"), "new\n").unwrap();
    std::fs::write(app.join("data.txt"), "new\n").unwrap();
    std::fs::write(repo.path().join("top.txt"), "new\n").unwrap();
    std::fs::write(repo.path().join("elsewhere.txt"), "outside the flow\n").unwrap();
    let flot = |args: &[&str]| {
        let output = Command::new(flot_bin())
            .args(args)
            .current_dir(&app)
            .env(
                "FLOT_SIGNING_KEY",
                repo.path().join(".test-keys/signing.key"),
            )
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    flot(&["run"]);
    let verified = flot(&["verify"]);
    assert!(
        verified.contains("changes no step read: data.txt, lib.txt\n"),
        "{verified}"
    );
}

#[test]
fn plan_holds_proofs_to_the_revision_proofs_from_names() {
    let project = Project::new(&ANCHORED.replace("proofs_from = \"main\"\n", ""));
    project.write("check.sh", "grep -q 1 out.txt\n");
    project.write(".gitignore", ".flot\nout.txt\n");
    git_in(project.path(), &["init", "-q", "-b", "main"]);
    commit(project.path(), "base");
    let main = git_in(project.path(), &["rev-parse", "main"]);
    project.write("check.sh", "true\n");
    project.flot(&["plan"]).ok().lacks("the proof differs");
    project
        .flot(&["plan", "--proofs-from", "main"])
        .ok()
        .has(&format!(
            "        ! the proof differs from main ({}) (check.sh differs); its claims would stay unverified\n",
            &main[..12]
        ));
}

#[test]
fn a_proof_held_to_no_revision_says_so_and_names_what_changed_in_it() {
    let project = Project::new(&ANCHORED.replace("proofs_from = \"main\"\n", ""));
    project.write("check.sh", "grep -q 1 out.txt\n");
    let first = project.flot(&["run"]);
    first
        .ok()
        .has("  - proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)")
        .lacks("its proof changed since");
    assert_eq!(first.stdout.matches("held to no revision").count(), 1);
    let before = run_id(&project);

    // The proof is weakened along with the work: the run still passes, and
    // names the change for whoever reads it.
    project.write("check.sh", "exit 0\n");
    let weakened = project.flot(&["run"]);
    weakened.ok().has(&format!(
        "  - work: its proof changed since run {before} (check.sh); read the change before trusting the verdict"
    ));
    let weakened_id = run_id(&project);
    project
        .flot(&["show", &weakened_id])
        .has(&format!("its proof changed since run {before}"));
    project
        .flot(&["verify", &weakened_id, "--markdown"])
        .has("its proof changed since run");
    // Against the run before it, nothing changed.
    project.flot(&["run"]).ok().lacks("its proof changed since");
    // Its command and its claims are the proof too.
    let unchanged = run_id(&project);
    project.write(
        "flot.toml",
        &ANCHORED
            .replace("proofs_from = \"main\"\n", "")
            .replace("verify = \"sh check.sh\"", "verify = \"true\"")
            .replace("out.txt holds 1", "it ran"),
    );
    project.flot(&["run"]).ok().has(&format!(
        "  - work: its proof changed since run {unchanged} (the verify command, the claims); read the change"
    ));

    // A flow that proves nothing has no proof to hold.
    Project::new(TWO_STEPS)
        .flot(&["run"])
        .ok()
        .lacks("held to no revision");

    // Held to a revision, the revision judges the change instead.
    let anchored = anchored_project();
    anchored.flot(&["run"]).ok().lacks("held to no revision");
    anchored.write("check.sh", "true\n");
    anchored
        .flot(&["run"])
        .has("the proof differs from main")
        .lacks("held to no revision")
        .lacks("its proof changed since");
}

#[test]
fn init_holds_proofs_to_the_remote_default_branch_when_there_is_one() {
    let seed = tempfile::tempdir().unwrap();
    git_in(seed.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(seed.path().join("README"), "seed\n").unwrap();
    commit(seed.path(), "seed");
    let remote = tempfile::tempdir().unwrap();
    let origin = remote.path().join("origin.git");
    git_in(
        remote.path(),
        &[
            "clone",
            "-q",
            "--bare",
            seed.path().to_str().unwrap(),
            "origin.git",
        ],
    );
    let project = Project {
        dir: tempfile::tempdir().unwrap(),
    };
    git_in(
        project.path(),
        &["clone", "-q", origin.to_str().unwrap(), "."],
    );
    project.flot(&["init"]).ok();
    let flow = read(&project, "flot.toml");
    assert!(flow.contains("\nproofs_from = \"origin/main\"\n"), "{flow}");
    // Until the flow is on that branch, nothing it claims is proven.
    let early = project.flot(&["run"]);
    assert_eq!(early.code, 2, "{}", early.stdout);
    early.has("the flow file is not there");
    commit(project.path(), "flow");
    git_in(project.path(), &["push", "-q", "origin", "main"]);
    project
        .flot(&["run"])
        .ok()
        .has("proofs checked against origin/main");

    // No remote to name: the setting is explained, not guessed.
    let alone = Project {
        dir: tempfile::tempdir().unwrap(),
    };
    git_in(alone.path(), &["init", "-q", "-b", "main"]);
    alone.flot(&["init"]).ok();
    let flow = read(&alone, "flot.toml");
    assert!(
        flow.contains("\n# proofs_from = \"origin/main\"\n"),
        "{flow}"
    );
    assert!(!flow.contains("\nproofs_from"), "{flow}");
    alone
        .flot(&["run"])
        .ok()
        .has("proofs are held to no revision");
}
