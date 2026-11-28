//! What a step's candidates leave: their receipts, which bind nothing of
//! the copies they ran in, and the kept one's outputs in the workspace.

use super::support::{forced, Workspace};
use crate::core::receipt::StepStatus;
use crate::engine::keying;
use crate::engine::runner::RunOptions;

/// What a candidate changes besides its outputs stays in its copy: even
/// a step held to its `may_change`, which snapshots the copy around its
/// command, binds none of it to the candidate's receipt.
#[test]
fn a_candidates_receipt_binds_nothing_it_changed_in_its_copy() {
    let workspace = Workspace::new(
        r#"
[flow]
name = "copies"

[[step]]
id = "pick"
run = '''printf '%s\n' "$FLOT_CANDIDATE" > pick.txt; printf 'note\n' > notes.txt'''
outputs = ["pick.txt"]
may_change = ["notes.txt"]
candidates = ["one", "two"]
"#,
    );
    let report = workspace.run(&RunOptions::default());
    let step = &report.manifest.steps[0];
    let receipts: Vec<&String> = std::iter::once(&step.receipt_id)
        .chain(
            step.candidates
                .iter()
                .map(|candidate| &candidate.receipt_id),
        )
        .collect();
    assert_eq!(receipts.len(), 2);
    for receipt_id in receipts {
        let identity = workspace.store.load_receipt(receipt_id).unwrap().identity;
        assert_eq!(
            identity.status,
            StepStatus::Ok,
            "{:?}",
            identity.limitations
        );
        assert_eq!(identity.outputs.len(), 1);
        assert!(identity.changed.is_empty(), "{:?}", identity.changed);
    }
    assert!(!workspace.root().join("notes.txt").exists());
}

/// The kept candidate's executable output is written back executable, both
/// where the workspace had no such file and over the one it had.
#[test]
fn a_kept_candidates_executable_output_is_executable_in_the_workspace() {
    let workspace = Workspace::new(
        r##"
[flow]
name = "tools"

[[step]]
id = "tool"
run = '''printf '#!/bin/sh\necho %s\n' "$FLOT_CANDIDATE" > tool.sh; chmod +x tool.sh'''
outputs = ["tool.sh"]
candidates = ["one", "two"]
"##,
    );
    let tool = workspace.root().join("tool.sh");
    let first = workspace.run(&RunOptions::default());
    assert!(first.steps[0].outputs[0].executable);
    assert!(keying::is_executable(&tool));
    let again = workspace.run(&forced());
    assert!(!again.steps[0].cached);
    assert!(keying::is_executable(&tool));
    assert!(std::fs::read_to_string(&tool)
        .unwrap()
        .starts_with("#!/bin/sh\necho "));
}
