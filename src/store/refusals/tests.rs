//! Tests of noted refusals: which ones a run records, and a note that cannot
//! be read.

use std::fs;

use super::super::tests::store;
use super::REFUSALS;
use crate::core::receipt::Refusal;

fn refusal(at_ms: u64) -> Refusal {
    Refusal {
        at_ms,
        tool: "Edit".into(),
        reasons: vec![format!("refused at {at_ms}")],
    }
}

#[test]
fn a_run_records_the_refusals_after_the_last_run_and_up_to_its_end() {
    let (_dir, store) = store();
    for at_ms in [30, 10, 40, 20] {
        store.note_refusal(&refusal(at_ms)).unwrap();
    }
    let noted = |after_ms, until_ms| -> Vec<u64> {
        store
            .refusals(after_ms, until_ms)
            .unwrap()
            .iter()
            .map(|refusal| refusal.at_ms)
            .collect()
    };
    // The one at the last run's end was its own.
    assert_eq!(noted(10, 30), [20, 30]);
    assert_eq!(noted(0, 40), [10, 20, 30, 40]);
    assert_eq!(store.refusals(10, 20).unwrap(), [refusal(20)]);
}

#[test]
fn refusals_that_cannot_be_read_are_an_error_not_none() {
    let (_dir, store) = store();
    assert!(
        store.refusals(0, u64::MAX).unwrap().is_empty(),
        "none noted"
    );
    fs::create_dir(store.root().join(REFUSALS)).unwrap();
    let error = store.refusals(0, u64::MAX).unwrap_err().to_string();
    assert!(error.contains("cannot read"), "{error}");
}
