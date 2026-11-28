//! A proof held to the mutants of the revision it tests.

use crate::support::{commit, git_in, Project};

/// A step whose proof is tested against mutants: `mutate.sh` stands for a
/// mutation tester, reporting in the schema Stryker writes one mutant of
/// `app.sh`, caught only by a test that checks the text exactly.
const MUTANTS: &str = r#"
[flow]
name = "mutants"
proofs_from = "main"

[[step]]
id = "s"
run = "true"
inputs = ["app.sh"]
claims = ["app.sh says hello"]
verify = "sh tests/check.sh"
verify_inputs = ["tests/**"]
mutants = { run = "sh mutate.sh", report = "report.json", allow = "allow.toml" }
"#;

fn mutants_project() -> Project {
    let project = Project::new(MUTANTS);
    project.write("app.sh", "echo hello\n");
    project.write(
        "tests/check.sh",
        "sh app.sh | grep -qx hello || { echo app.sh does not say hello; exit 1; }\n",
    );
    project.write(
        "mutate.sh",
        concat!(
            "status=Killed\n",
            "grep -q -- -qx tests/check.sh || status=Survived\n",
            "echo \"1 mutant of app.sh: $status\"\n",
            "[ -n \"${FLOT_PROOFS_FROM:-}\" ] && echo \"against ${FLOT_PROOFS_FROM%${FLOT_PROOFS_FROM#????????????}}\"\n",
            "cat > \"$FLOT_MUTANTS_DIR/report.json\" <<EOF\n",
            "{\"schemaVersion\": \"1\", \"thresholds\": {\"high\": 80, \"low\": 60}, \"files\": {\"app.sh\": {\"language\": \"shell\",\n",
            "\"source\": \"echo hello\\\\n\", \"mutants\": [{\"id\": \"1\", \"mutatorName\": \"StringLiteral\", \"replacement\": \"hell\",\n",
            "\"status\": \"$status\", \"location\": {\"start\": {\"line\": 1, \"column\": 6}, \"end\": {\"line\": 1, \"column\": 11}}},\n",
            "{\"id\": \"2\", \"mutatorName\": \"Command\", \"replacement\": \"true\", \"status\": \"CompileError\",\n",
            "\"location\": {\"start\": {\"line\": 1, \"column\": 1}, \"end\": {\"line\": 1, \"column\": 5}}}]}}}\n",
            "EOF\n",
        ),
    );
    project.write(".gitignore", ".flot\n.test-keys/\n");
    git_in(project.path(), &["init", "-q", "-b", "main"]);
    commit(project.path(), "base");
    project
}

#[test]
fn a_proof_that_lets_a_mutant_survive_proves_nothing_until_a_person_allows_it() {
    let project = mutants_project();
    let head = git_in(project.path(), &["rev-parse", "HEAD"]);
    let passed = project.flot(&["run"]);
    passed.ok().has("passed");
    project
        .flot(&["show", "latest"])
        .ok()
        .has("        mutants: 1 caught, 0 survived, 1 untested");
    project
        .flot(&["log", "s", "--mutants"])
        .ok()
        .has("1 mutant of app.sh: Killed")
        .has(&format!("against {}", &head.trim()[..12]));

    // A test that only checks the text is there passes as well, and lets the
    // mutant through.
    project.write(
        "tests/check.sh",
        "sh app.sh | grep -q hell || { echo app.sh does not say hello; exit 1; }\n",
    );
    commit(project.path(), "a looser test");
    let survived = project.flot(&["run"]);
    survived
        .partial()
        .has("claims 0v/1u/0r")
        .has("1 mutant survived the proof: app.sh:1: StringLiteral: hell; `flot show` names each as the allow list does")
        .lacks("allow.toml` matched no readable file")
        .lacks("passed");
    // The digest of the code the mutant changes, `hello`.
    let code = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    project
        .flot(&["show", "latest"])
        .has("        mutants: 0 caught, 1 survived, 1 untested")
        .has(&format!(
            "          survived at app.sh:1: {{ file = \"app.sh\", mutation = \"StringLiteral: hell\", code = \"{code}\" }}"
        ));
    project
        .flot(&["log", "s", "--mutants"])
        .ok()
        .has("1 mutant of app.sh: Survived");

    // The agent allows it: an edit to the proof, refused until the revision
    // has it.
    let entry = format!(
        "[[allow]]\nfile = \"app.sh\"\nmutation = \"StringLiteral: hell\"\ncode = \"{code}\"\nreason = \"hell is as good as hello\"\n"
    );
    project.write("allow.toml", &entry);
    let edited = project.flot(&["run"]);
    edited
        .partial()
        .has("the proof differs from main")
        .has("allow.toml is not there");

    // A person commits it: verified, then reused.
    commit(project.path(), "allow the shorter greeting");
    project.flot(&["run"]).ok().has("passed");
    project
        .flot(&["show", "latest"])
        .has("        mutants: 0 caught, 0 survived, 1 allowed, 1 untested")
        .has("          allowed at app.sh:1:");
    project.flot(&["run"]).ok().has("cached   s");
    // Tested against the changes since another revision, it is tested again.
    project.write("notes.txt", "unrelated\n");
    commit(project.path(), "an unrelated commit");
    project
        .flot(&["run"])
        .ok()
        .has("ran      s")
        .has("cached receipt was not tested against mutants with this revision");

    // The step's mutation tester is part of the proof as the revision has it.
    project.write("flot.toml", &MUTANTS.replace("sh mutate.sh", "true"));
    project
        .flot(&["run"])
        .partial()
        .has("the proof differs from main")
        .has("its mutants differ");
    project.write("flot.toml", MUTANTS);

    // `flot verify` holds a receipt to the mutation tester the revision has.
    project.flot(&["run"]).ok().has("cached   s");
    project.flot(&["verify"]).ok();
    project.write(
        "flot.toml",
        &MUTANTS.replace("sh mutate.sh", "sh ./mutate.sh"),
    );
    commit(project.path(), "another mutation command");
    project
        .flot(&["verify"])
        .has("its proof was tested against mutants by another command");
    project.write("flot.toml", MUTANTS);
    commit(project.path(), "the mutation command as it was");

    // An entry without a reason is no entry.
    project.write(
        "allow.toml",
        &entry.replace("hell is as good as hello", " "),
    );
    commit(project.path(), "no reason");
    project
        .flot(&["run"])
        .partial()
        .has("the mutation report shows nothing: in allow.toml: the allow list's entry for `StringLiteral: hell` in app.sh gives no reason");
}

#[test]
fn a_receipt_is_held_to_the_mutants_the_revision_tests() {
    // Proven before the revision declared mutants: not tested against them.
    let untested = mutants_project();
    let plain: String = MUTANTS
        .lines()
        .filter(|line| !line.starts_with("mutants ="))
        .map(|line| format!("{line}\n"))
        .collect();
    untested.write("flot.toml", &plain);
    commit(untested.path(), "no mutants yet");
    untested.flot(&["run"]).ok();
    untested.write("flot.toml", MUTANTS);
    commit(untested.path(), "mutants");
    untested
        .flot(&["verify"])
        .has("its proof was not tested against mutants");

    // A claim that did not verify is not asked how.
    let refuted = mutants_project();
    refuted.write("flot.toml", &plain);
    refuted.write("app.sh", "echo bye\n");
    commit(refuted.path(), "no mutants yet, and broken");
    let run = refuted.flot(&["run"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    refuted.write("flot.toml", MUTANTS);
    commit(refuted.path(), "mutants");
    refuted.flot(&["verify"]).lacks("tested against mutants");
}

#[test]
fn a_mutation_run_that_writes_no_report_or_another_tests_nothing() {
    let project = mutants_project();
    project.write("mutate.sh", "echo crashed; exit 4\n");
    commit(project.path(), "a tester that crashes");
    project
        .flot(&["run"])
        .partial()
        .has("the mutation report shows nothing: the run exited 4 and wrote no report at $FLOT_MUTANTS_DIR/report.json");

    project.write(
        "mutate.sh",
        "echo '{\"schemaVersion\": \"1\", \"files\": {\"app.sh\": {\"source\": \"x\", \"mutants\": [{\"id\": \"1\", \"mutatorName\": \"M\", \"status\": \"Pending\", \"location\": {\"start\": {\"line\": 1, \"column\": 1}, \"end\": {\"line\": 1, \"column\": 2}}}]}}}' > \"$FLOT_MUTANTS_DIR/report.json\"\n",
    );
    commit(project.path(), "a tester stopped half way");
    project
        .flot(&["run"])
        .partial()
        .has("mutant 1 of app.sh was never tested");

    project.write("mutate.sh", "mkdir \"$FLOT_MUTANTS_DIR/report.json\"\n");
    commit(project.path(), "a report that is not a file");
    project
        .flot(&["run"])
        .partial()
        .has("cannot read $FLOT_MUTANTS_DIR/report.json");

    // cargo-mutants' outcomes, with the list of mutants it was to test beside
    // them: a survivor is named by its description, without its place, and
    // its code read from the file the report names.
    let cargo = |file: &str| {
        format!(
            concat!(
                "m='{{\"name\": \"{file}:1:6: replace greeting with hell\", \"package\": \"p\", \"file\": \"{file}\", \"genre\": \"FnValue\", \"replacement\": \"hell\", ",
                "\"span\": {{\"start\": {{\"line\": 1, \"column\": 6}}, \"end\": {{\"line\": 1, \"column\": 11}}}}}}'\n",
                "echo \"[$m]\" > \"$FLOT_MUTANTS_DIR/mutants.json\"\n",
                "echo \"{{\\\"cargo_mutants_version\\\": \\\"27.1.0\\\", \\\"outcomes\\\": [{{\\\"scenario\\\": \\\"Baseline\\\", \\\"summary\\\": \\\"Success\\\"}}, ",
                "{{\\\"scenario\\\": {{\\\"Mutant\\\": $m}}, \\\"summary\\\": \\\"MissedMutant\\\"}}]}}\" > \"$FLOT_MUTANTS_DIR/report.json\"\n",
            ),
            file = file
        )
    };
    project.write("mutate.sh", &cargo("app.sh"));
    commit(project.path(), "cargo-mutants");
    project
        .flot(&["run"])
        .partial()
        .has("1 mutant survived the proof: app.sh:1: replace greeting with hell;");
    let digest = project.flot(&["show", "latest"]);
    // None untested, none said.
    digest
        .has("mutants: 0 caught, 1 survived\n")
        .lacks("untested");
    digest.has(&format!(
        "mutation = \"replace greeting with hell\", code = \"sha256:{}\"",
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    ));
    // A file outside the flow root is not read.
    project.write("mutate.sh", &cargo("../app.sh"));
    commit(project.path(), "cargo-mutants elsewhere");
    project
        .flot(&["run"])
        .partial()
        .has("cannot read ../app.sh, which the report names");

    // A run that rewrites what the proof read judges nothing.
    project.write("mutate.sh", "echo changed >> tests/check.sh\n");
    commit(project.path(), "a tester that edits the tests");
    project
        .flot(&["run"])
        .partial()
        .has("the mutation run changed tests/check.sh, which the verdict judged");

    // Not tested when the proof fails: refuted as it was.
    let failing = mutants_project();
    failing.write("app.sh", "echo bye\n");
    let refuted = failing.flot(&["run"]);
    assert_eq!(refuted.code, 1, "{}", refuted.stdout);
    failing
        .flot(&["log", "s", "--mutants"])
        .has("tested no mutants: it declares none, or its proof did not pass first");
}
