//! Tests of what a run's explanation says, on runs made here, with the
//! helpers they share.

use serde_json::json;

use crate::core::receipt::{
    ClaimRecord, ClaimStatus, Observed, Receipt, ReceiptIdentity, StepStatus, ToolIdentity,
};
use crate::engine::runner::UNCONFINED;
use crate::service::audit::Outcome;
use crate::service::{RunOptions, Workspace};

use super::causes::causes;
use super::{Cause, Kind};

mod proofs;

fn workspace(flow: &str) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("flot.toml"), flow).unwrap();
    let workspace = Workspace::at(dir.path(), &dir.path().join("flot.toml"));
    (dir, workspace)
}

/// Each step explained, as `(step, kind, detail, next)`.
fn said(workspace: &Workspace, run: &str) -> Vec<(String, Kind, String, String)> {
    let explanation = workspace.explain(run, None).unwrap();
    explanation
        .steps
        .into_iter()
        .flat_map(|step| {
            step.causes
                .into_iter()
                .map(move |cause| (step.step_id.clone(), cause.kind, cause.detail, cause.next))
        })
        .collect()
}

fn cause(step: &str, kind: Kind, detail: &str, next: &str) -> (String, Kind, String, String) {
    (step.into(), kind, detail.into(), next.into())
}

#[test]
fn a_run_that_passed_has_nothing_to_explain_and_one_of_some_steps_says_so() {
    let (_dir, workspace) = workspace(
        "[flow]\nname = \"t\"\n\n[[step]]\nid = \"a\"\nrun = \"true\"\nclaims = [\"it holds\"]\nverify = \"true\"\n\n[[step]]\nid = \"c\"\nrun = \"true\"\n",
    );
    workspace
        .run(&RunOptions {
            only: Some("a".into()),
            ..RunOptions::default()
        })
        .unwrap();
    let partial = workspace.explain("latest", None).unwrap();
    assert_eq!(partial.result, Outcome::Partial);
    assert_eq!(
        partial.causes,
        [Cause::new(
            Kind::Partial,
            "only some of the flow's steps ran, so the flow as a whole is not proven",
            "flot run"
        )]
    );
    assert!(partial.steps.is_empty());

    workspace.run(&RunOptions::default()).unwrap();
    let explanation = workspace.explain("latest", None).unwrap();
    assert_eq!(explanation.result, Outcome::Passed);
    assert!(explanation.causes.is_empty() && explanation.steps.is_empty());
    // Asked about a step that held, it is named, with nothing against it.
    let step = workspace.explain("latest", Some("a")).unwrap();
    assert_eq!(step.steps.len(), 1);
    assert!(step.steps[0].causes.is_empty());
    let error = workspace.explain("latest", Some("b")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("has no step `b`; its steps are a, c"),
        "{error}"
    );
}

#[test]
fn each_step_says_what_kept_it_from_holding_and_what_to_read_next() {
    let (_dir, workspace) = workspace(
        r#"
[flow]
name = "t"

[[step]]
id = "fails"
run = "exit 3"

[[step]]
id = "waits"
run = "true"
needs = ["fails"]
claims = ["it holds"]
verify = "true"

[[step]]
id = "refuted"
run = "true"
claims = ["it holds", "so does this"]
verify = "exit 4"

[[step]]
id = "unproven"
run = "true"
claims = ["it is safe", "it is fast"]

[[step]]
id = "tolerated"
run = "exit 5"
allow_failure = true
"#,
    );
    let run = workspace.run(&RunOptions::default()).unwrap();
    let explanation = workspace.explain("latest", None).unwrap();
    assert_eq!(explanation.result, Outcome::Failed);
    assert!(explanation.causes.is_empty(), "{:?}", explanation.causes);
    assert_eq!(
        said(&workspace, "latest"),
        [
            cause(
                "fails",
                Kind::Failed,
                "its command exited 3",
                "flot log fails --stderr"
            ),
            // Two claims a proof refutes are one cause.
            cause(
                "refuted",
                Kind::Refuted,
                "its proof exited 4",
                "flot log refuted --proof"
            ),
            cause(
                "tolerated",
                Kind::Failed,
                "its command exited 5; allow_failure kept the run going, but proves nothing",
                "flot log tolerated --stderr"
            ),
            // Two claims of one kind for different reasons are two.
            cause(
                "unproven",
                Kind::NoProof,
                "no verify command proves \"it is safe\"",
                "declare `verify` for the step in the flow"
            ),
            cause(
                "unproven",
                Kind::NoProof,
                "no verify command proves \"it is fast\"",
                "declare `verify` for the step in the flow"
            ),
            cause(
                "waits",
                Kind::NotRun,
                "dependency fails (failed) did not hold",
                "flot explain"
            ),
        ]
    );
    // Asked about a run by name, the commands name it too.
    let named = said(&workspace, &run.manifest.run_id);
    assert_eq!(
        named[0].3,
        format!("flot log fails --stderr {}", run.manifest.run_id)
    );
}

#[test]
fn a_timeout_and_an_altered_receipt_are_explained() {
    let (dir, workspace) = workspace(
        "[flow]\nname = \"t\"\n\n[[step]]\nid = \"slow\"\nrun = \"sleep 5\"\nbudget = { timeout_secs = 1 }\n\n[[step]]\nid = \"quick\"\nrun = \"true\"\n",
    );
    let run = workspace.run(&RunOptions::default()).unwrap();
    let explanation = workspace.explain("latest", None).unwrap();
    assert_eq!(explanation.steps.len(), 1, "{:?}", explanation.steps);
    assert_eq!(explanation.steps[0].causes[0].kind, Kind::TimedOut);
    assert!(explanation.steps[0].causes[0]
        .detail
        .starts_with("stopped at its 1s timeout"));

    let quick = &run
        .manifest
        .steps
        .iter()
        .find(|step| step.step_id == "quick")
        .unwrap()
        .receipt_id;
    let path = std::fs::read_dir(dir.path().join(".flot/receipts"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| quick.ends_with(path.file_stem().unwrap().to_str().unwrap()))
        .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replacen("\"true\"", "\"false\"", 1)).unwrap();
    let explanation = workspace.explain("latest", None).unwrap();
    assert_eq!(explanation.result, Outcome::Broken);
    assert_eq!(explanation.causes[0].kind, Kind::Broken);
    assert!(explanation
        .steps
        .iter()
        .any(|step| step.step_id == "quick" && step.causes[0].kind == Kind::Altered));
    // What it serializes to is what an agent reads.
    let value = serde_json::to_value(&explanation).unwrap();
    assert_eq!(value["result"], "broken");
    assert_eq!(value["causes"][0]["kind"], json!("broken"));
}

/// A step whose sandbox cannot be enforced here did not run: its cause is
/// the host, and `flot doctor` says what it lacks. No host the suite runs
/// on lacks it, so the receipt is the one the runner records.
#[test]
fn a_step_whose_sandbox_cannot_be_enforced_points_at_doctor() {
    let why = "could not run: its sandbox cannot be enforced here: Landlock ABI 2";
    let receipt = Receipt::seal(
        ReceiptIdentity {
            flow: "t".into(),
            step_id: "confined".into(),
            step_key: "key".into(),
            command: "true".into(),
            status: StepStatus::Failed,
            exit_code: None,
            stdout_digest: String::new(),
            stderr_digest: String::new(),
            inputs: Vec::new(),
            verify_inputs: Vec::new(),
            outputs: Vec::new(),
            changed: Default::default(),
            deps: Vec::new(),
            claims: vec![ClaimRecord {
                text: "it holds".into(),
                status: ClaimStatus::Unverified,
                verify_command: Some("true".into()),
                verify_exit: None,
                verify_output_digest: None,
                reason: Some(UNCONFINED.into()),
                before: None,
                held: None,
                mutants: None,
                differed: None,
            }],
            limitations: vec![why.into()],
            tool: ToolIdentity::default(),
            candidate: None,
            sandbox: None,
        },
        Observed::default(),
    );
    assert_eq!(
        causes(&receipt, ""),
        [Cause::new(Kind::Sandbox, why, "flot doctor")]
    );
}
