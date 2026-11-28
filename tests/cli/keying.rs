//! What keys a step on its inputs: the executable bit git records, and what a file holds now.

use crate::support::{git_in, Project};

#[cfg(unix)]
use crate::support::{commit, CANDIDATES};

/// On any platform, a file git records as executable is keyed so where git
/// does not trust the file system, as a Linux or macOS checkout keys it.
#[test]
fn a_file_git_records_as_executable_is_keyed_so() {
    let project = Project::new(
        "[flow]\nname = \"modes\"\n\n[[step]]\nid = \"plain\"\ninputs = [\"run.sh\"]\nrun = \"true\"\n",
    );
    project.write("run.sh", "true\n");
    project.write(".gitignore", ".flot/\n");
    git_in(project.path(), &["init", "-q"]);
    git_in(project.path(), &["config", "core.fileMode", "false"]);
    git_in(project.path(), &["add", "-A"]);
    git_in(project.path(), &["update-index", "--chmod=+x", "run.sh"]);
    project.flot(&["run"]).ok();
    let show = project.flot(&["--json", "show"]).json();
    let receipt_id = show["steps"][0]["receipt_id"].as_str().unwrap().to_string();
    let receipt = project.flot(&["receipt", &receipt_id]).json();
    assert_eq!(
        receipt["identity"]["inputs"][0]["executable"],
        serde_json::json!(true),
        "{receipt}"
    );
}

/// Where git does not trust the file system's executable bit (Windows, WSL's
/// `/mnt` drives, where every file looks executable), a step is keyed by the
/// bit git records, so its key matches a Linux or macOS checkout's.
#[cfg(unix)]
#[test]
fn where_git_ignores_the_executable_bit_the_key_uses_the_recorded_one() {
    use std::os::unix::fs::PermissionsExt;
    let project = Project::new(&format!(
        "{CANDIDATES}\n[[step]]\nid = \"plain\"\ninputs = [\"question\"]\nrun = \"true\"\n"
    ));
    project.write("question", "life\n");
    project.write(".gitignore", ".flot/\n");
    git_in(project.path(), &["init", "-q"]);
    commit(project.path(), "start");
    let key = |project: &Project| {
        let plan = project.flot(&["--json", "plan"]).json();
        let steps = plan["steps"].as_array().unwrap().clone();
        let plain = steps
            .iter()
            .find(|step| step["step_id"] == "plain")
            .unwrap();
        plain["step_key"].as_str().unwrap().to_string()
    };
    let chmod = |mode: u32| {
        for file in ["question", "flot.toml", ".gitignore"] {
            std::fs::set_permissions(
                project.path().join(file),
                std::fs::Permissions::from_mode(mode),
            )
            .unwrap();
        }
    };
    let checkout = key(&project);
    // Trusted, the file system's bit keys the step.
    chmod(0o777);
    assert_ne!(key(&project), checkout);
    // Not trusted, the recorded one does, as in the checkout.
    git_in(project.path(), &["config", "core.fileMode", "false"]);
    assert_eq!(key(&project), checkout);
    git_in(project.path(), &["update-index", "--chmod=+x", "question"]);
    assert_ne!(key(&project), checkout);
    git_in(project.path(), &["update-index", "--chmod=-x", "question"]);
    // A candidate's copy has no history: it is keyed by the workspace's, so
    // the receipt it made is reused.
    project
        .flot(&["run"])
        .ok()
        .has("kept cheap")
        .lacks("changed its own declared inputs");
    project
        .flot(&["run"])
        .ok()
        .has("cached   solve")
        .lacks("kept");
}

/// A step rewrites a file at its old size and puts its old modification time
/// back; the step after it, in the same run, keys on what the file holds now,
/// not on what the run read of it before.
#[cfg(unix)]
#[test]
fn a_file_rewritten_with_its_old_size_and_time_is_read_again_in_the_same_run() {
    let project = Project::new(
        r#"
[flow]
name = "rewritten"

[[step]]
id = "edit"
run = "touch -r data.txt was && printf new > data.txt && touch -r was data.txt && rm was"
inputs = ["stamp.txt"]
may_change = ["data.txt", "was"]

[[step]]
id = "read"
needs = ["edit"]
inputs = ["data.txt"]
run = "cat data.txt"
"#,
    );
    project.write("stamp.txt", "stamp\n");
    project.write("data.txt", "old");
    project.flot(&["run"]).ok().has("ran      read");
    // A second process reads the file afresh: had the first keyed `read` on
    // the old content, its receipt would not answer now.
    project.flot(&["run"]).ok().has("cached   read");
}
