//! What a step must and must not do with the outputs it declares.

use crate::support::Project;

#[test]
fn a_success_that_skips_a_declared_output_fails() {
    let project = Project::new(
        r#"
[flow]
name = "forgetful"

[[step]]
id = "make"
run = "echo done"
outputs = ["report.txt"]
claims = ["the report exists"]
verify = "true"
"#,
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output
        .has("FAILED")
        .has("produced no file for declared output report.txt")
        .has("claims 0v/1u/0r");
}

#[test]
fn unordered_steps_producing_one_file_are_reported() {
    let project = Project::new(
        r#"
[flow]
name = "clash"

[[step]]
id = "a"
run = "echo a > shared.txt"
outputs = ["shared.txt"]

[[step]]
id = "b"
run = "echo b > shared.txt"
outputs = ["shared.txt"]

[[step]]
id = "c"
run = "echo c > shared.txt"
needs = ["a", "b"]
outputs = ["shared.txt"]
"#,
    );
    project
        .flot(&["run"])
        .ok()
        .has("steps a and b both produce shared.txt and neither needs the other")
        .lacks("steps a and c")
        .lacks("steps b and c");
}

#[test]
fn outputs_must_stay_inside_the_flow_and_off_the_proof() {
    for (outputs, verify_inputs, message) in [
        (
            "[\"../escape.txt\"]",
            "[]",
            "must be a relative path inside the flow root",
        ),
        (
            "[\"/tmp/abs.txt\"]",
            "[]",
            "must be a relative path inside the flow root",
        ),
        (
            "[\"check.sh\"]",
            "[\"check.sh\"]",
            "a step cannot produce its own proof",
        ),
    ] {
        let project = Project::new(&format!(
            "[flow]\nname = \"bad\"\n\n[[step]]\nid = \"s\"\nrun = \"true\"\nclaims = [\"x\"]\nverify = \"sh check.sh\"\noutputs = {outputs}\nverify_inputs = {verify_inputs}\n"
        ));
        let output = project.flot(&["plan"]);
        assert_eq!(output.code, 3, "{outputs}: {}", output.stderr);
        output.has(message);
    }
}

#[test]
fn a_new_file_under_an_output_pattern_is_not_answered_by_the_receipt() {
    let project = Project::new(
        r#"
[flow]
name = "gen"

[[step]]
id = "gen"
run = "mkdir -p out; echo ok > out/good.txt"
outputs = ["out/*.txt"]
claims = ["no bad output"]
verify = "test ! -e out/bad.txt"
"#,
    );
    project.flot(&["run"]).ok().has("PASSED");
    project.write("out/bad.txt", "x\n");
    let run = project.flot(&["run"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    run.has("ran      gen")
        .has("gen: out/bad.txt matches an output pattern but is not in its receipt; the step runs again")
        .has("FAILED");
    // Without it, the receipt that held is reused, and a missing output
    // is still put back.
    std::fs::remove_file(project.path().join("out/bad.txt")).unwrap();
    std::fs::remove_file(project.path().join("out/good.txt")).unwrap();
    project
        .flot(&["run"])
        .ok()
        .has("cached   gen")
        .has("output out/good.txt was missing and was restored from its receipt");
}
