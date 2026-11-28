//! Tests of the allow list and of the code a span covers; each report
//! reader is tested in its own file.

use super::place::span;
use super::*;

mod cargo;
mod schema;

#[test]
fn an_entry_allows_the_survivor_it_names_and_no_other() {
    let survivor = |mutation: &str, code: &str| Survivor {
        file: "src/a.rs".into(),
        mutation: mutation.into(),
        code: code.into(),
        line: 1,
    };
    let allowed = read_allow(
        "[[allow]]\nfile = \"src/a.rs\"\nmutation = \"replace > with >=\"\ncode = \"sha256:1\"\nreason = \"equivalent: the age is never equal\"\n",
    )
    .unwrap();
    let (left, kept) = judge(
        vec![
            survivor("replace > with >=", "sha256:1"),
            survivor("replace > with >=", "sha256:2"),
            survivor("replace > with <", "sha256:1"),
        ],
        &allowed,
    );
    assert_eq!(kept, [survivor("replace > with >=", "sha256:1")]);
    assert_eq!(left.len(), 2);
    assert_eq!(read_allow("").unwrap(), []);

    for (text, refusal) in [
        (
            "[[allow]]\nfile = \"a\"\nmutation = \"m\"\ncode = \"c\"\nreason = \" \"\n",
            "gives no reason",
        ),
        (
            "[[allow]]\nfile = \"a\"\nmutation = \"m\"\ncode = \"c\"\n",
            "missing field `reason`",
        ),
        (
            "[[allow]]\nfile = \"a\"\nmutation = \"m\"\ncode = \"c\"\nreason = \"r\"\nline = 3\n",
            "unknown field `line`",
        ),
    ] {
        let error = format!("{:#}", read_allow(text).unwrap_err());
        assert!(error.contains(refusal), "{text}: {error}");
    }
}

#[test]
fn a_span_counts_characters_and_excludes_its_end() {
    assert_eq!(span("añb = 1", (1, 2), (1, 4)).as_deref(), Some("ñb"));
    assert_eq!(span("ab", (1, 1), (1, 3)).as_deref(), Some("ab"));
    assert_eq!(span("ab", (1, 1), (1, 4)), None);
    assert_eq!(span("ab", (2, 1), (2, 2)), None);
    assert_eq!(span("ab\ncd", (2, 1), (1, 2)), None);
    assert_eq!(span("ab", (0, 1), (1, 2)), None);
}
