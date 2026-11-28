//! What `verify` checks of a recorded run and how it says so.

use crate::support::{anchored_project, commit, receipt_file, Project, PROVEN, TWO_STEPS};

#[test]
fn verify_authenticates_a_recorded_run() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    project
        .flot(&["verify"])
        .ok()
        .has("integrity: intact")
        .has("verdict passed");
}

#[test]
fn verify_detects_an_edited_receipt() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();

    let receipts = project.path().join(".flot/receipts");
    let entry = std::fs::read_dir(&receipts)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = std::fs::read_to_string(&entry).unwrap();
    let forged = text
        .replace("\"unverified\"", "\"verified\"")
        .replace("\"exit_code\": 0", "\"exit_code\": 0, \"forged\": true");
    std::fs::write(&entry, forged.replace("echo flot", "echo tampered")).unwrap();

    let output = project.flot(&["verify"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.has("integrity: BROKEN").has("does not match its id");
}

#[test]
fn verify_recomputes_the_verdict_instead_of_reading_it() {
    let project = Project::new(
        "[flow]\nname = \"v\"\n\n[[step]]\nid = \"s\"\nrun = \"true\"\nclaims = [\"x\"]\nverify = \"false\"\n",
    );
    assert_eq!(project.flot(&["run"]).code, 1);
    let latest = std::fs::read_to_string(project.path().join(".flot/latest")).unwrap();
    let run_file = project.path().join(format!(".flot/runs/{latest}.json"));
    let mut run: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&run_file).unwrap()).unwrap();
    run["verdict"] = "passed".into();
    std::fs::write(&run_file, run.to_string()).unwrap();

    let output = project.flot(&["verify"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.has("manifest verdict is passed but its receipts make it failed");
}

#[test]
fn a_receipt_file_holding_another_receipt_fails_verify() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let (a, b) = (receipt_file(&project, 0), receipt_file(&project, 1));
    assert!(a.exists() && b.exists(), "{a:?}");
    std::fs::copy(&b, &a).unwrap();
    let output = project.flot(&["verify"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.lacks("integrity: intact").has("holds receipt");
}

#[test]
fn verify_reads_every_evidence_object_not_just_its_name() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let run = project.flot(&["show", "--json"]).json();
    let receipt = project.flot(&["receipt", run["steps"][0]["receipt_id"].as_str().unwrap()]);
    let stdout = receipt.json()["identity"]["stdout_digest"]
        .as_str()
        .unwrap()
        .trim_start_matches("sha256:")
        .to_string();
    std::fs::write(
        project
            .path()
            .join(".flot/objects")
            .join(&stdout[..2])
            .join(&stdout),
        "tampered\n",
    )
    .unwrap();
    let output = project.flot(&["verify"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.lacks("integrity: intact").has("is not intact");
}

#[test]
fn a_tampered_receipt_is_refused_by_name_not_printed() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let path = receipt_file(&project, 0);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replacen("echo first", "echo forged", 1)).unwrap();
    let id = project.flot(&["show", "--json"]).json()["steps"][0]["receipt_id"]
        .as_str()
        .unwrap()
        .to_string();
    let output = project.flot(&["receipt", &id]);
    assert_eq!(output.code, 1);
    assert!(output.stdout.is_empty());
    output.has("does not match its body");
    // `show` does not repeat what the altered receipt says either.
    let detail = project.flot(&["show", "--json"]).json();
    assert!(detail["step_claims"].get("a").is_none(), "{detail}");
    assert!(detail["step_claims"].get("b").is_some(), "{detail}");
}

#[test]
fn verify_states_one_result_and_its_exit_code_follows_it() {
    for (flow, result, code) in [
        (PROVEN, "passed", 0),
        (&*PROVEN.replace("verify = ", "# "), "unverified", 2),
        (
            &*PROVEN.replace("grep -q flot", "grep -q nope"),
            "failed",
            1,
        ),
    ] {
        let project = Project::new(flow);
        project.write("seed.txt", "one");
        project.flot(&["run"]);
        let audit = project.flot(&["--json", "verify"]);
        assert_eq!(audit.code, code, "{}", audit.stdout);
        assert_eq!(audit.json()["result"], result, "{}", audit.stdout);
    }

    // Evidence that no longer hashes to its name is broken, whatever the
    // altered record says its verdict was.
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    let mut objects = vec![project.path().join(".flot/objects")];
    while let Some(dir) = objects.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                objects.push(path);
                continue;
            }
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(&path, permissions).unwrap();
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.extend_from_slice(b"x\n");
            std::fs::write(&path, bytes).unwrap();
        }
    }
    let broken = project.flot(&["--json", "verify"]);
    assert_eq!(broken.code, 1, "{}", broken.stdout);
    let audit = broken.json();
    assert_eq!(audit["intact"], false);
    assert_eq!(audit["verdict"], "passed");
    assert_eq!(audit["result"], "broken");
    project
        .flot(&["verify", "--markdown"])
        .has("### flot: evidence broken");

    // A run that passed, held to a revision whose proof says otherwise, is
    // unverified.
    let anchored = anchored_project();
    anchored.flot(&["run"]).ok();
    anchored.write("check.sh", "grep -q 1 out.txt # stricter\n");
    commit(anchored.path(), "stricter proof");
    let drifted = anchored.flot(&["--json", "verify"]);
    assert_eq!(drifted.code, 2, "{}", drifted.stdout);
    let audit = drifted.json();
    assert_eq!(audit["verdict"], "passed");
    assert_eq!(audit["result"], "unverified");
}

#[test]
fn verify_says_what_the_run_could_not_prove() {
    let project = Project::new(
        "[flow]\nname = \"v\"\n\n[[step]]\nid = \"work\"\nrun = \"echo 1 > out.txt\"\nclaims = [\"out holds 1\"]\nverify = \"grep -q 1 out.txt\"\n",
    );
    project.flot(&["run"]).ok();
    project
        .flot(&["verify"])
        .ok()
        .has("verdict passed")
        .has("limitations:")
        .has("proofs are held to no revision");
}
