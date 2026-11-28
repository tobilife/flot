//! What a plan predicts without running anything: what a run would reuse,
//! and how many files each step and its proof read.

use super::support::{forced, Workspace};
use crate::engine::baseline::Baseline;
use crate::engine::runner::RunOptions;

const CHECKED: &str = r#"
[flow]
name = "checked"

[[step]]
id = "check"
run = "true"
inputs = ["src.txt", "lib.txt"]
claims = ["it checks"]
verify = "sh check.sh"
verify_inputs = ["check.sh"]
"#;

fn checked() -> Workspace {
    let workspace = Workspace::new(CHECKED);
    workspace.write("src.txt", "fn main() {}\n");
    workspace.write("lib.txt", "pub fn lib() {}\n");
    workspace.write("check.sh", "exit 0\n");
    workspace
}

#[test]
fn a_plan_counts_the_files_a_step_and_its_proof_read() {
    let workspace = checked();
    let plan = workspace.runner().plan(&RunOptions::default()).unwrap();
    assert_eq!(plan[0].inputs, 3);
}

#[test]
fn a_forced_plan_predicts_no_reuse() {
    let workspace = checked();
    let ran = workspace.run(&RunOptions::default());
    let key = workspace.receipt(&ran, "check").identity.step_key;
    let plan = workspace.runner().plan(&RunOptions::default()).unwrap();
    assert!(plan[0].cached);
    let forced_plan = workspace.runner().plan(&forced()).unwrap();
    assert!(!forced_plan[0].cached);
    assert_eq!(forced_plan[0].step_key.as_deref(), Some(key.as_str()));
}

/// A receipt whose proof differs from the revision proofs are held to does
/// not stand in for running its step, so a plan does not predict it will.
#[test]
fn a_plan_predicts_no_reuse_of_a_proof_that_differs_from_its_revision() {
    let workspace = checked();
    workspace.write("flot.toml", CHECKED);
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(workspace.root())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    // The proof as it is here, which the run proves the step with.
    workspace.write("check.sh", "true\n");
    let ran = workspace.run(&RunOptions::default());
    assert!(workspace.receipt(&ran, "check").is_reusable());
    assert!(workspace.runner().plan(&RunOptions::default()).unwrap()[0].cached);

    let baseline = Baseline::load(workspace.root(), "flot.toml", "HEAD").unwrap();
    let plan = workspace
        .runner()
        .with_baseline(Some(&baseline))
        .plan(&RunOptions::default())
        .unwrap();
    assert!(!plan[0].cached);
    assert!(
        plan[0]
            .limitations
            .iter()
            .any(|said| said.starts_with("the proof differs from HEAD (")),
        "{:?}",
        plan[0].limitations
    );
}
