//! The copy a candidate works in.

use crate::support::Project;

#[test]
fn a_candidate_works_inside_the_store_and_cannot_reach_the_repository() {
    let project = Project::new(
        r#"
[flow]
name = "where"

[[step]]
id = "look"
candidates = ["a", "b"]
run = """
if test -f ../../FORMAT; then echo store > where; else pwd > where; fi
git rev-parse --show-toplevel > repo 2>&1 || echo none > repo
"""
outputs = ["where", "repo"]
"#,
    );
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(project.path())
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "-q"]);
    project.flot(&["run"]).ok();
    let read = |name: &str| std::fs::read_to_string(project.path().join(name)).unwrap();
    // In the store, so on the workspace's own filesystem, where its files
    // read back alike.
    assert_eq!(read("where"), "store\n");
    assert_eq!(read("repo"), "none\n");
    let left: Vec<_> = std::fs::read_dir(project.path().join(".flot").join("tmp"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    assert!(
        left.is_empty(),
        "workspace copies outlived the run: {left:?}"
    );
}

/// Both candidates answer honestly; the check is found by a wildcard.
const HONEST: &str = r#"
[flow]
name = "copies"

[[step]]
id = "solve"
candidates = ["first", "second"]
run = "echo good > answer"
outputs = ["answer"]
claims = ["the answer is good"]
verify = "sh checks/check.sh"
verify_inputs = ["checks/*.sh"]
"#;

#[test]
fn a_candidate_copy_reads_the_files_the_project_does() {
    let project = Project::new(HONEST);
    project.write("checks/check.sh", "grep -q good answer\n");
    project
        .flot(&["run"])
        .ok()
        .has("PASSED")
        .lacks("matched no readable file");
    project.flot(&["run"]).ok().has("cached   solve");

    // A candidate that rewrites the check in its copy changed its own proof.
    let cheat = Project::new(&HONEST.replace(
        "run = \"echo good > answer\"",
        "run = \"echo bad > answer; echo true > checks/check.sh\"",
    ));
    cheat.write("checks/check.sh", "grep -q good answer\n");
    let run = cheat.flot(&["run"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    run.has("UNVERIFIED")
        .has("the step changed its own proof (checks/check.sh)");
}
