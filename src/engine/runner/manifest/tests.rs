//! The start a run's manifest records, kept after the run before it.

use super::after_the_run_before;

#[test]
fn a_run_starts_after_the_one_recorded_before_it() {
    assert_eq!(after_the_run_before(500, None), (500, None));
    assert_eq!(after_the_run_before(500, Some(499)), (500, None));
    let (start, note) = after_the_run_before(500, Some(500));
    assert_eq!(start, 501);
    assert!(note.unwrap().contains("(0 ms earlier)"));
    let (start, note) = after_the_run_before(300, Some(1000));
    assert_eq!(start, 1001);
    assert!(note.unwrap().contains("(700 ms earlier)"));
}
