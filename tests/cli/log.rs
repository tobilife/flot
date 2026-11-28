//! A step's complete output: kept out of the inline view, read by `log`, found by `search`.

use crate::support::{run_id, Project, CANDIDATES, RETRYING};

#[test]
fn complete_output_stays_retrievable_when_the_inline_view_is_trimmed() {
    let project = Project::new(
        r#"
[flow]
name = "budget"
budget = { max_inline_bytes = 16 }

[[step]]
id = "chatty"
run = "yes flot | head -c 5000"
claims = ["the step produced output"]
verify = "true"
"#,
    );
    let run = project.flot(&["run", "--json"]);
    run.ok();
    let step = &run.json()["steps"][0];
    assert_eq!(step["stdout_truncated"], serde_json::json!(true));
    assert_eq!(step["stdout_inline"].as_str().unwrap().len(), 16);

    let receipt_id = step["receipt_id"].as_str().unwrap().to_string();
    let receipt = project.flot(&["receipt", &receipt_id]);
    receipt.ok();
    let object = receipt.json()["identity"]["stdout_digest"]
        .as_str()
        .unwrap()
        .to_string();
    let complete = project.flot(&["cat", &object]);
    complete.ok();
    assert_eq!(
        complete.stdout.len(),
        5000,
        "the complete stream is recoverable"
    );
}

#[test]
fn the_truncation_hint_names_a_command_that_prints_all_of_it() {
    let project = Project::new(
        r#"
[flow]
name = "hint"
budget = { max_inline_bytes = 8 }

[[step]]
id = "chatty"
run = "yes flot | head -c 300"
"#,
    );
    let run = project.flot(&["run"]);
    run.ok();
    // Trimming is how the output is shown, not a limit of the evidence: the
    // hint says where the rest is, and no limitation repeats it.
    run.lacks("trimmed");
    let hint = run
        .stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("complete output: flot "))
        .expect("a trimmed step prints a recovery hint");
    let args: Vec<&str> = hint.split_whitespace().collect();
    assert_eq!(project.flot(&args).ok().stdout.len(), 300);
}

#[test]
fn log_prints_a_steps_complete_output_and_show_points_a_failure_at_it() {
    let project = Project::new(
        r#"
[flow]
name = "loud"

[[step]]
id = "noisy"
run = "for i in $(seq 1 3000); do echo line $i; done; echo oops >&2; exit 1"
"#,
    );
    let run = project.flot(&["run"]);
    assert_eq!(run.code, 1, "stderr:\n{}", run.stderr);
    // The run shows where the output starts and where it ends, not all of it.
    run.has("    | line 1\n")
        .has("    | line 3000\n")
        .lacks("    | line 1500\n")
        .has("output omitted here")
        .has("complete output: flot log noisy");
    let log = project.flot(&["log", "noisy"]);
    log.ok().has("line 1\n").has("line 3000\n");
    project.flot(&["log", "noisy", "--stderr"]).ok().has("oops");
    let id = run_id(&project);
    project
        .flot(&["show"])
        .has(&format!("output: flot log noisy {id}"));
    let missing = project.flot(&["log", "quiet"]);
    assert_eq!(missing.code, 3, "stderr:\n{}", missing.stderr);
    missing.has("its steps are noisy");
    let unnamed = project.flot(&["log"]);
    assert_eq!(unnamed.code, 3, "stderr:\n{}", unnamed.stderr);
    assert!(unnamed.stdout.is_empty(), "{}", unnamed.stdout);
    unnamed.has(&format!(
        "name the step whose output to print; the steps of run {id} are noisy"
    ));
}

#[test]
fn log_prints_what_a_refuted_proof_printed_and_the_run_points_at_it() {
    let project = Project::new(
        r#"
[flow]
name = "proof"

[[step]]
id = "check"
run = "echo built"
claims = ["the check holds"]
verify = "echo expected 3 >&2; echo got 2; exit 1"

[[step]]
id = "later"
run = "true"
needs = ["check"]
claims = ["later holds"]
verify = "true"

[[step]]
id = "bare"
run = "true"
"#,
    );
    let run = project.flot(&["run"]);
    assert_eq!(run.code, 1, "stderr:\n{}", run.stderr);
    let id = run_id(&project);
    // Only a step with a refuted claim is pointed at its proof's output.
    run.has(&format!("    proof output: flot log check --proof {id}"))
        .lacks("flot log later --proof")
        .lacks("flot log bare --proof");
    let proof = project.flot(&["log", "check", "--proof"]);
    proof.ok().has("expected 3\n").has("got 2\n").lacks("built");
    project
        .flot(&["log", "check"])
        .ok()
        .has("built")
        .lacks("got 2");
    project
        .flot(&["show"])
        .has(&format!("      proof output: flot log check --proof {id}"))
        .lacks("flot log later --proof")
        .lacks("flot log bare --proof");
    let unrun = project.flot(&["log", "later", "--proof"]);
    assert_eq!(unrun.code, 3, "stderr:\n{}", unrun.stderr);
    assert!(unrun.stdout.is_empty(), "{}", unrun.stdout);
    unrun.has(&format!("step `later` of run {id} ran no proof: "));
    project
        .flot(&["log", "bare", "--proof"])
        .has("ran no proof: it makes no claims");
    let both = project.flot(&["log", "check", "--proof", "--stderr"]);
    assert_eq!(both.code, 3, "stderr:\n{}", both.stderr);
}

#[test]
fn search_finds_what_runs_recorded_and_names_where() {
    let project = Project::new(CANDIDATES);
    project.write("question", "life\n");
    project.flot(&["run"]).ok();
    // A candidate not kept is searched, with the line its output said it on.
    project
        .flot(&["search", "WRONG READ"])
        .ok()
        .has("  solve candidate wrong  stdout:1  wrong read life")
        .has("1 hit(s) in 1 run(s) searched");
    // A reused step's hits belong to the run that executed it.
    project.flot(&["run"]).ok().has("cached   solve");
    project
        .flot(&["search", "cheap read"])
        .ok()
        .has("1 hit(s) in 2 run(s) searched");
    // Claims with their verdicts, and the run's own limitations.
    project
        .flot(&["search", "the answer is 42"])
        .ok()
        .has("  solve candidate wrong  claim  the answer is 42 [refuted]");
    project
        .flot(&["search", "kept cheap"])
        .ok()
        .has("  (run)  limitation  solve: candidates wrong refuted");
    let limited = project.flot(&["search", "read life", "--limit", "2"]);
    limited
        .ok()
        .has("2 hit(s) in 2 run(s) searched; more hits exist, raise --limit");
    let json = project
        .flot(&["--json", "search", "pricey read"])
        .ok()
        .json();
    assert_eq!(json["hits"][0]["candidate"], "pricey");
    assert_eq!(json["hits"][0]["source"], "stdout");
    assert_eq!(json["hits"][0]["line"], 1);
    assert_eq!(json["runs_searched"], 2);
    assert_eq!(json["truncated"], false);
    project
        .flot(&["search", "no such text"])
        .ok()
        .has("0 hit(s) in 2 run(s) searched");

    let project = Project::new(&RETRYING.replace("GOAL", "2"));
    project.flot(&["run"]).ok();
    project
        .flot(&["search", "count is 1"])
        .ok()
        .has("  fix attempt 1  proof:1  count is 1");
}
