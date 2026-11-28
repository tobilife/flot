//! Results that change on the same inputs.

use crate::support::Project;

const COIN: &str = r#"
[flow]
name = "coin"

[[step]]
id = "toss"
run = "n=$(cat count 2>/dev/null || echo 0); echo $((n+1)) > count; [ $((n % 2)) -eq 0 ]"
# Unverified when the step fails: that is the step's flakiness, not the proof's.
claims = ["the toss is recorded"]
verify = "test -f count"

[[step]]
id = "proof"
run = "true"
claims = ["the coin lands heads"]
verify = "n=$(cat heads 2>/dev/null || echo 0); echo $((n+1)) > heads; [ $((n % 2)) -eq 0 ]"

[[step]]
id = "steady"
run = "echo same"
claims = ["it says same"]
verify = "true"
"#;

#[test]
fn flaky_names_what_changed_its_result_on_the_same_inputs() {
    let project = Project::new(COIN);
    project.flot(&["flaky"]).ok().has("in 0 run(s) read");
    project.flot(&["run"]);
    project
        .flot(&["flaky"])
        .ok()
        .has("0 step(s) and 0 proof(s) gave different results on the same inputs in 1 run(s) read");
    project.flot(&["run", "--force"]);
    project.flot(&["run", "--force"]);
    let newest = project.flot(&["show", "--json"]).json()["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let flaky = project.flot(&["flaky"]);
    assert_eq!(flaky.code, 2, "{}", flaky.stdout);
    flaky
        .has("step `toss` (key ")
        .has(&format!("  ok        in 2 runs, newest {newest}"))
        .has("  failed    in 1 run, newest ")
        .has("proof of \"the coin lands heads\" (step `proof`, key ")
        .has("  verified  in 2 runs, newest ")
        .has("  refuted   in 1 run, newest ")
        .has("1 step(s) and 1 proof(s) gave different results on the same inputs in 3 run(s) read")
        .lacks("steady");
    let json = project.flot(&["flaky", "--json"]).json();
    assert_eq!(json["steps"][0]["step_id"], "toss");
    assert_eq!(json["claims"][0]["claim"], "the coin lands heads");
    // Fewer runs, less history.
    project.flot(&["flaky", "--runs", "1"]).ok();

    // A run from another host is not history of this one.
    project
        .flot(&["bundle", "-o", "coin.json", "--allow-secrets"])
        .ok();
    let other = Project::new(COIN);
    other
        .flot(&["import", project.path().join("coin.json").to_str().unwrap()])
        .ok();
    other.flot(&["flaky"]).ok().has("in 0 run(s) read");
    project
        .flot(&["doctor"])
        .has("warn  history         1 step and 1 proof changed their result on the same inputs");
}
