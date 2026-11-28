//! Candidates that answer a step apart, and the one that is kept.

use crate::support::{public_key, Project, CANDIDATES, RETRYING};

#[test]
fn a_candidate_reused_from_a_signed_run_stays_its_signers_after_the_run() {
    let source = Project::new(CANDIDATES);
    source.write("question", "life\n");
    let key = public_key(&source);
    source.flot(&["run"]).ok().has("kept cheap");
    let bundle = source.path().join("signed.flotbundle.json");
    source
        .flot(&["bundle", "--sign", "-o", bundle.to_str().unwrap()])
        .ok();

    let target = Project::new(CANDIDATES);
    target.write("question", "life\n");
    target.flot(&["trust", "add", "alice", &key]).ok();
    target
        .flot(&["import", "--require-trusted", bundle.to_str().unwrap()])
        .ok();
    target
        .flot(&["run"])
        .ok()
        .has("cached   solve")
        .has("candidate cheap  vouched for by alice");
    // The run that reused alice's candidate remembered it as hers, so
    // revoking her stops the reuse on the next run too.
    target.flot(&["trust", "remove", "alice"]).ok();
    target
        .flot(&["run"])
        .ok()
        .has("ran      solve")
        .has("kept cheap")
        .lacks("cached   solve");
}

#[test]
fn candidates_run_apart_and_the_cheapest_that_holds_is_kept() {
    let project = Project::new(CANDIDATES);
    project.write("question", "life\n");
    let run = project.flot(&["run"]);
    run.ok()
        .has("candidate cheap")
        .has("cheap read life")
        .has("using 42")
        .has("solve: candidates wrong refuted (cost unknown), pricey held ($0.500000), cheap held ($0.100000); kept cheap")
        .has("spent $0.600000");
    assert_eq!(
        std::fs::read_to_string(project.path().join("answer")).unwrap(),
        "42\n"
    );
    for name in ["wrong", "pricey", "cheap"] {
        assert!(
            !project.path().join(format!("stray-{name}")).exists(),
            "{name} wrote into the project"
        );
    }
    project
        .flot(&["show"])
        .ok()
        .has("      candidate wrong  receipt ")
        .has("      candidate pricey  receipt ");
    project
        .flot(&["verify"])
        .ok()
        .has("receipts checked 4")
        .has("integrity: intact");
    project.flot(&["plan"]).ok().has("reuse solve");
    project
        .flot(&["run"])
        .ok()
        .has("cached   solve")
        .has("candidate cheap")
        .lacks("kept");

    // The losers travel with the run.
    project
        .flot(&["bundle", "latest~1", "-o", "run.json"])
        .ok()
        .has("(4 receipts,");
    let target = Project::new(CANDIDATES);
    target
        .flot(&["import", project.path().join("run.json").to_str().unwrap()])
        .ok();
}

#[test]
fn when_no_candidate_holds_the_first_is_kept_and_the_run_fails() {
    let project = Project::new(&CANDIDATES.replace("echo 42", "echo 43"));
    project.write("question", "life\n");
    let run = project.flot(&["run"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    run.has("kept wrong").has("candidate wrong");
}

#[test]
fn each_candidate_makes_its_own_attempts() {
    let project = Project::new(
        &CANDIDATES
            .replace("candidates = [\"wrong\", \"pricey\", \"cheap\"]", "candidates = [\"wrong\", \"late\"]\nattempts = 2")
            .replace(
                "  cheap)  echo 42",
                "  late)   test -e tried && echo 42 > answer || { touch tried; echo 40 > answer; } ;;\n  cheap)  echo 42",
            ),
    );
    project.write("question", "life\n");
    project
        .flot(&["run"])
        .ok()
        .has("candidates wrong refuted after 2 attempts (cost unknown), late held after 2 attempts (cost unknown); kept late");
    project.flot(&["verify"]).ok().has("integrity: intact");
}

#[test]
fn the_cost_ceiling_stops_starting_candidates() {
    let project = Project::new(
        &CANDIDATES
            .replace(
                "[\"wrong\", \"pricey\", \"cheap\"]",
                "[\"pricey\", \"cheap\"]",
            )
            .replace(
                "name = \"candidates\"",
                "name = \"candidates\"\nmax_cost_usd = 0.4",
            ),
    );
    project.write("question", "life\n");
    let run = project.flot(&["run"]);
    // The ceiling holds back the next step too.
    assert_eq!(run.code, 1, "{}", run.stdout);
    run.has("candidate cheap not run: run cost ceiling of $0.400000 reached ($0.500000 spent by executed steps)")
        .has("kept pricey");
}

#[test]
fn candidates_are_two_or_more_distinct_names() {
    for (flow, error) in [
        (
            CANDIDATES.replace("[\"wrong\", \"pricey\", \"cheap\"]", "[\"only\"]"),
            "names one candidate",
        ),
        (
            CANDIDATES.replace("[\"wrong\", \"pricey\", \"cheap\"]", "[\"a\", \"a\"]"),
            "names candidate `a` twice",
        ),
        (
            CANDIDATES.replace("[\"wrong\", \"pricey\", \"cheap\"]", "[\"a\", \"b c\"]"),
            "candidate `b c` must use only",
        ),
        (
            CANDIDATES.replace(
                "outputs = [\"answer\"]",
                "outputs = [\"answer\"]\nenv = { FLOT_CANDIDATE = \"x\" }",
            ),
            "sets FLOT_CANDIDATE",
        ),
    ] {
        let run = Project::new(&flow).flot(&["run"]);
        assert_eq!(run.code, 3, "{}", run.stderr);
        run.has(error);
    }
}

#[test]
fn a_report_without_a_cost_is_unknown_not_free() {
    let project = Project::new(
        &CANDIDATES
            .replace("[\"wrong\", \"pricey\", \"cheap\"]", "[\"tokens\", \"pricey\"]")
            .replace("name = \"candidates\"", "name = \"candidates\"\nmax_cost_usd = 5")
            .replace(
                "  cheap)  echo 42",
                "  tokens) echo 42 > answer; echo '{\"input_tokens\": 5}' > \"$FLOT_METRICS\" ;;\n  cheap)  echo 42",
            ),
    );
    project.write("question", "life\n");
    project
        .flot(&["run"])
        .ok()
        .has("candidates tokens held (cost unknown), pricey held ($0.500000); kept pricey")
        .has("2 reporting step(s), 1 silent, 1 without cost)")
        .has("1 executed step(s) reported no cost (solve (tokens)); the cost ceiling cannot see what they spent");
}

#[test]
fn log_reads_a_candidate_not_kept_and_an_attempt_that_did_not_hold() {
    let project = Project::new(CANDIDATES);
    project.write("question", "life\n");
    project.flot(&["run"]).ok();
    project
        .flot(&["log", "solve"])
        .ok()
        .has("cheap read life")
        .lacks("wrong");
    project
        .flot(&["log", "solve", "--candidate", "wrong"])
        .ok()
        .has("wrong read life");
    project
        .flot(&["log", "solve", "--candidate", "cheap"])
        .ok()
        .has("cheap read life");
    let missing = project.flot(&["log", "solve", "--candidate", "nobody"]);
    assert_ne!(missing.code, 0);
    missing.has("ran no candidate `nobody`; it ran cheap, wrong, pricey");
    let plain = project.flot(&["log", "use", "--candidate", "wrong"]);
    assert_ne!(plain.code, 0);
    plain.has("step `use` of run ").has(" has no candidates");

    let project = Project::new(&RETRYING.replace("GOAL", "2"));
    project.flot(&["run"]).ok();
    project.flot(&["log", "fix"]).ok().has("attempt 2 of 3");
    project
        .flot(&["log", "fix", "--attempt", "1"])
        .ok()
        .has("attempt 1 of 3")
        .lacks("attempt 2");
    let beyond = project.flot(&["log", "fix", "--attempt", "3"]);
    assert_ne!(beyond.code, 0);
    beyond
        .has("attempt 3 is not recorded: step `fix` of run ")
        .has(" took 2 attempt(s)");
}

#[test]
fn a_candidate_whose_attempt_reported_no_cost_ranks_after_a_known_cost() {
    let project = Project::new(
        r#"
[flow]
name = "partial"

[[step]]
id = "solve"
candidates = ["patchy", "steady"]
attempts = 2
run = """
case "$FLOT_CANDIDATE-$FLOT_ATTEMPT" in
  patchy-1) echo 41 > answer ;;
  patchy-2) echo 42 > answer; echo '{"cost_usd": 0.1}' > "$FLOT_METRICS" ;;
  steady-*) echo 42 > answer; echo '{"cost_usd": 0.5}' > "$FLOT_METRICS" ;;
esac
"""
outputs = ["answer"]
claims = ["the answer is 42"]
verify = "test $(cat answer) = 42"
"#,
    );
    project
        .flot(&["run"])
        .ok()
        .has("candidates patchy held after 2 attempts (cost unknown), steady held ($0.500000); kept steady");
    project.flot(&["verify"]).ok().has("integrity: intact");
}
