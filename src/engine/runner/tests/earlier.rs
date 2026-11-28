//! What a run takes from the runs before it: the files a step's earlier
//! receipts answer for that are still as it left them, and the proof each
//! step last ran with here.

use super::support::{forced, Workspace};
use crate::engine::runner::{RunOptions, RunReport};

/// A step that finds its work done answers for it, as long as the file is
/// still what it left; once someone else changed it, it is theirs.
#[test]
fn a_step_answers_for_work_it_finds_done_only_while_it_is_in_place() {
    let workspace = Workspace::new(
        r#"
[flow]
name = "settle"

[[step]]
id = "settle"
run = '''test -f state.txt || printf 'a\n' > state.txt'''
"#,
    );
    let first = workspace.run(&RunOptions::default());
    let made = workspace.receipt(&first, "settle").identity.changed;
    assert_eq!(made.keys().collect::<Vec<_>>(), ["state.txt"]);

    let again = workspace.run(&forced());
    assert_eq!(workspace.receipt(&again, "settle").identity.changed, made);

    workspace.write("state.txt", "b\n");
    let after = workspace.run(&forced());
    let changed = workspace.receipt(&after, "settle").identity.changed;
    assert!(changed.is_empty(), "{changed:?}");
}

/// What a recorded run names for a step is not the step's to answer for
/// when it is another step's receipt.
#[test]
fn a_step_does_not_answer_for_what_another_steps_receipt_left() {
    let workspace = Workspace::new(
        r#"
[flow]
name = "two"

[[step]]
id = "make"
run = '''test -f made.txt || printf 'made\n' > made.txt'''

[[step]]
id = "idle"
run = "true"
"#,
    );
    let first = workspace.run(&RunOptions::default());
    let made = workspace.receipt(&first, "make");
    assert!(made.identity.changed.contains_key("made.txt"));
    assert!(workspace
        .receipt(&first, "idle")
        .identity
        .changed
        .is_empty());
    // The run as recorded, with `make`'s receipt named for `idle`.
    let mut recorded = workspace.store.resolve_run("latest").unwrap();
    for entry in &mut recorded.steps {
        if entry.step_id == "idle" {
            entry.receipt_id = made.receipt_id.clone();
        }
    }
    workspace.store.save_run(&recorded).unwrap();

    let second = workspace.run(&forced());
    let changed = workspace.receipt(&second, "idle").identity.changed;
    assert!(changed.is_empty(), "{changed:?}");
    assert!(workspace
        .receipt(&second, "make")
        .identity
        .changed
        .contains_key("made.txt"));
}

const PROVEN: &str = r#"
[flow]
name = "proven"

[[step]]
id = "first"
run = "true"
claims = ["first holds"]
verify = "sh check.sh"
verify_inputs = ["check.sh"]

[[step]]
id = "second"
run = "true"
claims = ["second holds"]
verify = "true"
"#;

/// A step's proof is compared with the one it ran with in the newest run
/// here that ran it, though that run left out other steps found further
/// back.
#[test]
fn a_proof_is_compared_with_the_newest_run_that_has_it() {
    let workspace = Workspace::new(PROVEN);
    workspace.write("check.sh", "exit 0\n");
    let told = |report: &RunReport| -> Vec<String> {
        let entry = report.steps.iter().find(|entry| entry.step_id == "first");
        entry.unwrap().limitations.clone()
    };
    let first = workspace.run(&RunOptions::default());

    workspace.write("check.sh", "true\n");
    let only = RunOptions {
        only: Some("first".into()),
        ..RunOptions::default()
    };
    let changed = workspace.run(&only);
    let since = format!(
        "its proof changed since run {} (check.sh)",
        first.manifest.run_id
    );
    assert!(
        told(&changed).iter().any(|said| said.starts_with(&since)),
        "{:?}",
        told(&changed)
    );

    let whole = workspace.run(&RunOptions::default());
    assert!(
        !told(&whole)
            .iter()
            .any(|said| said.starts_with("its proof changed")),
        "{:?}",
        told(&whole)
    );
}
