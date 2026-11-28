//! A proof that differs from its baseline's, and what is said of it.

use crate::support::{anchored_project, git_in, ANCHORED};

#[test]
fn a_proof_that_differs_from_the_baseline_proves_nothing() {
    let project = anchored_project();
    project
        .flot(&["run"])
        .ok()
        .has("proofs checked against main");
    project
        .flot(&["verify", "--proofs-from", "main"])
        .ok()
        .has("proofs: as at main");

    // The proof is weakened in the working copy: the step runs, the proof does
    // not, and no receipt made before is reused.
    project.write("check.sh", "true\n");
    let weakened = project.flot(&["run"]);
    assert_eq!(weakened.code, 2, "{}", weakened.stdout);
    weakened
        .has("claims 0v/1u/0r")
        .has("the proof differs from main")
        .has("check.sh differs");
    // Its claim stays unverified, and the change to its proof is all that
    // keeps the run from passing: what a person reviews.
    project
        .flot(&["verify", "--proofs-from", "main"])
        .has("proofs: NOT as at main")
        .has("only the proofs differ: no step failed, and every claim left unverified is one of a step whose proof changed; a person reviews that change, which no run proves");
    project
        .flot(&["plan"])
        .has("its claims would stay unverified");

    // Dropping the setting from the working copy does not drop the check: the
    // committed flow keeps it.
    project.write(
        "flot.toml",
        &ANCHORED.replace("proofs_from = \"main\"\n", ""),
    );
    let dropped = project.flot(&["run"]);
    assert_eq!(dropped.code, 2, "{}", dropped.stdout);

    // Committed on another branch, the weaker proof passes its own run, and
    // `verify` against main still refuses it.
    project.write("flot.toml", ANCHORED);
    let dir = project.path();
    git_in(dir, &["checkout", "-q", "-b", "agent"]);
    git_in(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qam",
            "weaken",
        ],
    );
    project.write("flot.toml", &ANCHORED.replace("main", "agent"));
    git_in(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qam",
            "own baseline",
        ],
    );
    project.flot(&["run"]).ok();
    let held = project.flot(&["verify", "--proofs-from", "main"]);
    assert_eq!(held.code, 2, "{}", held.stdout);
    held.has("proofs: NOT as at main")
        .has("work: check.sh differs")
        .has("only the proofs differ: no step failed, and every claim left unverified is one of a step whose proof changed; a person reviews that change, which no run proves");
    let report = project.flot(&["verify", "--proofs-from", "main", "--markdown"]);
    assert_eq!(report.code, 2, "{}", report.stdout);
    report
        .has("### flot: unverified, only the proofs differ from main")
        .has("recorded verdict passed")
        .has("Only the proofs differ: no step failed, and every claim left unverified is one of a step whose proof changed; a person reviews that change, which no run proves\n")
        .has("- Proofs: **not as at main");
    let audit = project.flot(&["verify", "--json", "--proofs-from", "main"]);
    assert_eq!(audit.code, 2, "{}", audit.stdout);
    assert_eq!(audit.json()["only_proofs_differ"], true, "{}", audit.stdout);
    assert_eq!(audit.json()["result"], "unverified", "{}", audit.stdout);
    // Nor does a receipt that passed against its own baseline stand in.
    let reused = project.flot(&["run", "--proofs-from", "main"]);
    assert_eq!(reused.code, 2, "{}", reused.stdout);
    reused.has("ran      work").lacks("cached ");

    // A claim dropped from the flow still counts against it.
    project.write(
        "flot.toml",
        "[flow]\nname = \"anchored\"\n\n[[step]]\nid = \"other\"\nrun = \"true\"\n",
    );
    project
        .flot(&["run", "--proofs-from", "main"])
        .has("steps with claims at main")
        .has("work; their claims are not made");

    let missing = project.flot(&["run", "--proofs-from", "nowhere"]);
    assert_eq!(missing.code, 4, "{}", missing.stderr);
    missing.has("which is not a commit in this repository");
}

#[test]
fn verify_names_a_change_to_a_proof_that_its_receipt_does_not_keep() {
    let project = anchored_project();
    // Holding the step's files is a change to its proof that no receipt
    // records: the run found it, and did not run the proof.
    project.write(
        "flot.toml",
        &ANCHORED.replace(
            "verify_inputs = [\"check.sh\"]",
            "verify_inputs = [\"check.sh\"]\nheld = [\"check.sh\"]",
        ),
    );
    let run = project.flot(&["run"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    run.has("the proof differs from main")
        .has("its held files differ");
    let held = project.flot(&["verify", "--proofs-from", "main"]);
    assert_eq!(held.code, 2, "{}", held.stdout);
    held.has("proofs: NOT as at main")
        .has("  - work: its held files differ\n")
        .has("only the proofs differ");

    // Against another commit, the run's own finding is not this one's: the
    // claim is unverified, and nothing says the proof changed.
    project.write("notes.txt", "later\n");
    let dir = project.path();
    git_in(dir, &["add", "notes.txt"]);
    git_in(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "later",
        ],
    );
    let later = project.flot(&["verify", "--json", "--proofs-from", "main"]);
    assert_eq!(later.code, 2, "{}", later.stdout);
    let later = later.json();
    assert_eq!(later["only_proofs_differ"], false, "{later}");
    assert_eq!(later["anchor"]["drifted"], serde_json::json!([]));
    assert_eq!(later["anchor"]["unverified"], serde_json::json!(["work"]));
}

#[test]
fn a_run_held_back_by_more_than_its_changed_proofs_says_so() {
    // At main, a second step whose claim no failure can prove.
    let flow = format!(
        "{ANCHORED}\n[[step]]\nid = \"flaky\"\nrun = \"exit 7\"\nallow_failure = true\nclaims = [\"it ran\"]\nverify = \"true\"\n"
    );
    let project = anchored_project();
    project.write("flot.toml", &flow);
    let dir = project.path();
    git_in(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qam",
            "flaky",
        ],
    );
    project.write("check.sh", "true\n");
    let run = project.flot(&["run"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    run.has("claims 0v/1u/0r");
    let audit = project.flot(&["verify", "--json", "--proofs-from", "main"]);
    assert_eq!(audit.code, 2, "{}", audit.stdout);
    let audit = audit.json();
    assert_eq!(audit["only_proofs_differ"], false, "{audit}");
    assert_eq!(audit["anchor"]["unverified"], serde_json::json!(["flaky"]));
    assert_eq!(audit["anchor"]["drifted"][0]["step_id"], "work");
    project
        .flot(&["verify", "--proofs-from", "main"])
        .has("proofs: NOT as at main")
        .lacks("only the proofs differ");
    project
        .flot(&["verify", "--markdown", "--proofs-from", "main"])
        .has("### flot: unverified\n")
        .lacks("nly the proofs differ");
}
