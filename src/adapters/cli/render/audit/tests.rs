//! Tests of how a report lists paths.

use super::listed;

fn paths(count: usize) -> Vec<String> {
    (0..count).map(|number| format!("f{number}")).collect()
}

const TWENTY: &str =
    "f0, f1, f2, f3, f4, f5, f6, f7, f8, f9, f10, f11, f12, f13, f14, f15, f16, f17, f18, f19";

/// Twenty paths are all named; from the twenty-first on, they are counted.
#[test]
fn up_to_twenty_paths_are_named_and_the_rest_counted() {
    assert_eq!(listed(&paths(19)), TWENTY.trim_end_matches(", f19"));
    assert_eq!(listed(&paths(20)), TWENTY);
    assert_eq!(listed(&paths(21)), format!("{TWENTY} and 1 more"));
    assert_eq!(listed(&paths(23)), format!("{TWENTY} and 3 more"));
}
