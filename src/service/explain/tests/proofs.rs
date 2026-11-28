//! What an explanation says of proofs held to a git revision: one that
//! changed, one that passes before the change, one that fails with the
//! files it holds, one that lets a mutant survive, and a run that only the
//! change to its proofs keeps from passing.

use crate::service::audit::Outcome;
use crate::service::RunOptions;

use super::super::Kind;
use super::{said, workspace};

const PROOFS: &str = r#"
[flow]
name = "proofs"
proofs_from = "main"

[[step]]
id = "drift"
run = "true"
claims = ["check.sh holds"]
verify = "sh check.sh"
verify_inputs = ["check.sh"]

[[step]]
id = "before"
run = "true"
claims = ["kept.txt is there"]
verify = "test -f kept.txt"
fails_before = true

[[step]]
id = "held"
run = "true"
claims = ["app.txt is fixed"]
verify = "sh tests/t.sh"
held = ["tests/**"]

[[step]]
id = "mutated"
run = "true"
claims = ["app.sh says hello"]
verify = "true"
mutants = { run = "sh mutate.sh", report = "report.json" }

[[step]]
id = "many"
run = "true"
claims = ["app.sh says hello, every way"]
verify = "true"
mutants = { run = "sh many.sh", report = "report.json" }
"#;

/// A mutation tester that finds one mutant of `app.sh` the proof misses,
/// in the schema Stryker writes.
const MUTATE: &str = r#"cat > "$FLOT_MUTANTS_DIR/report.json" <<'EOF'
{"schemaVersion": "1", "thresholds": {"high": 80, "low": 60}, "files": {"app.sh": {"language": "shell",
"source": "echo hello\n", "mutants": [{"id": "1", "mutatorName": "StringLiteral", "replacement": "hell",
"status": "Survived", "location": {"start": {"line": 1, "column": 6}, "end": {"line": 1, "column": 11}}}]}}}
EOF
"#;

/// A mutation tester that finds four mutants of `app.sh` the proof misses.
const MANY: &str = r#"cat > "$FLOT_MUTANTS_DIR/report.json" <<'EOF'
{"schemaVersion": "1", "thresholds": {"high": 80, "low": 60}, "files": {"app.sh": {"language": "shell",
"source": "echo hello\n", "mutants": [
{"id": "1", "mutatorName": "StringLiteral", "replacement": "hell", "status": "Survived", "location": {"start": {"line": 1, "column": 6}, "end": {"line": 1, "column": 11}}},
{"id": "2", "mutatorName": "StringLiteral", "replacement": "help", "status": "Survived", "location": {"start": {"line": 1, "column": 6}, "end": {"line": 1, "column": 11}}},
{"id": "3", "mutatorName": "StringLiteral", "replacement": "held", "status": "Survived", "location": {"start": {"line": 1, "column": 6}, "end": {"line": 1, "column": 11}}},
{"id": "4", "mutatorName": "StringLiteral", "replacement": "helm", "status": "Survived", "location": {"start": {"line": 1, "column": 6}, "end": {"line": 1, "column": 11}}}]}}}
EOF
"#;

/// Run git in `dir`, as a host with no git identity of its own.
fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", dir.join("no-global-gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn committed(flow: &str, files: &[(&str, &str)]) -> (tempfile::TempDir, crate::service::Workspace) {
    let (dir, workspace) = workspace(flow);
    let root = dir.path();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    for (path, text) in files {
        std::fs::write(root.join(path), text).unwrap();
    }
    std::fs::write(root.join(".gitignore"), ".flot\n").unwrap();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "base"]);
    (dir, workspace)
}

#[test]
fn each_proof_held_to_a_revision_says_how_it_did_not_hold() {
    let (dir, workspace) = committed(
        PROOFS,
        &[
            ("check.sh", "true\n"),
            ("kept.txt", "here\n"),
            ("tests/t.sh", "grep -q fixed app.txt\n"),
            ("app.txt", "broken\n"),
            ("app.sh", "echo hello\n"),
            ("mutate.sh", MUTATE),
            ("many.sh", MANY),
        ],
    );
    let root = dir.path();
    // The work weakens one proof and the tests another proof runs.
    std::fs::write(root.join("check.sh"), "true # weaker\n").unwrap();
    std::fs::write(root.join("tests/t.sh"), "true\n").unwrap();
    workspace.run(&RunOptions::default()).unwrap();

    let found = said(&workspace, "latest");
    let kinds: Vec<(&str, Kind)> = found
        .iter()
        .map(|(step, kind, _, _)| (step.as_str(), *kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("before", Kind::FailsBefore),
            ("drift", Kind::ProofChanged),
            ("held", Kind::Held),
            ("many", Kind::Mutants),
            ("mutated", Kind::Mutants),
        ],
        "{found:#?}"
    );
    // The run lists its steps in the order it ran them.
    let main = std::process::Command::new("git")
        .args(["rev-parse", "main"])
        .current_dir(root)
        .output()
        .unwrap();
    let main = String::from_utf8(main.stdout).unwrap();
    let (_, _, drift, next) = &found[1];
    assert!(
        drift.starts_with(&format!("its proof differs from {}: ", &main[..12])),
        "{drift}"
    );
    assert!(drift.contains("check.sh"), "{drift}");
    assert_eq!(
        next,
        "flot verify; a person reviews the change, which no run proves"
    );
    assert_eq!(
        found[0].2,
        format!(
            "the proof passes with the changes since {} undone, so it shows nothing of them",
            &main[..12]
        )
    );
    assert!(found[0].3.starts_with("flot cat sha256:"), "{found:#?}");
    assert!(
        found[2].2.contains("tests/t.sh as the revision has them"),
        "{found:#?}"
    );
    assert_eq!(found[2].3, "flot log held --held");
    // Three are named, and how many more there are.
    let (_, _, many, _) = &found[3];
    assert!(
        many.starts_with("4 mutants the proof did not catch: app.sh:1: "),
        "{many}"
    );
    assert_eq!(many.matches("app.sh:1: ").count(), 3, "{many}");
    assert!(many.ends_with("; and 1 more"), "{many}");
    let (_, _, one, next) = &found[4];
    assert!(
        one.starts_with("1 mutant the proof did not catch: app.sh:1: ") && !one.contains("more"),
        "{one}"
    );
    assert_eq!(next, "flot log mutated --mutants");
}

#[test]
fn a_run_only_the_change_to_a_proof_keeps_from_passing_says_so() {
    let (dir, workspace) = committed(
        &PROOFS[..PROOFS.find("[[step]]\nid = \"before\"").unwrap()],
        &[("check.sh", "true\n")],
    );
    workspace.run(&RunOptions::default()).unwrap();
    assert_eq!(
        workspace.explain("latest", None).unwrap().result,
        Outcome::Passed
    );
    std::fs::write(dir.path().join("check.sh"), "true # weaker\n").unwrap();
    workspace.run(&RunOptions::default()).unwrap();
    let explanation = workspace.explain("latest", None).unwrap();
    assert_eq!(explanation.result, Outcome::Unverified);
    let kinds: Vec<Kind> = explanation.causes.iter().map(|cause| cause.kind).collect();
    assert_eq!(kinds, [Kind::OnlyProofsDiffer]);
    assert_eq!(explanation.causes[0].next, "flot verify");
    assert_eq!(explanation.steps.len(), 1);
    assert_eq!(explanation.steps[0].causes[0].kind, Kind::ProofChanged);
}
