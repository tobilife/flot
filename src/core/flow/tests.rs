//! Tests of flows: the order and closure of their steps, their digest, the
//! files a step declares, budgets and unproven claims, with the helper the
//! flow tests share.

use anyhow::Result;

use super::*;

mod refusals;

pub(super) fn flow(text: &str) -> Result<Flow> {
    Flow::parse(text)
}

const BASIC: &str = r#"
[flow]
name = "demo"

[[step]]
id = "b"
run = "echo b"
needs = ["a"]

[[step]]
id = "a"
run = "echo a"
"#;

#[test]
fn orders_dependencies_before_dependents() {
    let flow = flow(BASIC).unwrap();
    assert_eq!(flow.topo_order().unwrap(), vec!["a", "b"]);
}

#[test]
fn rejects_cycles() {
    let err = flow(
        r#"
[flow]
name = "cyclic"

[[step]]
id = "a"
run = "echo a"
needs = ["b"]

[[step]]
id = "b"
run = "echo b"
needs = ["a"]
"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("dependency cycle"), "{err}");
}

#[test]
fn rejects_unknown_need() {
    let err = flow(
        r#"
[flow]
name = "x"

[[step]]
id = "a"
run = "echo a"
needs = ["ghost"]
"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("unknown step `ghost`"), "{err}");
}

#[test]
fn rejects_duplicate_ids() {
    let err = flow(
        r#"
[flow]
name = "x"

[[step]]
id = "a"
run = "echo 1"

[[step]]
id = "a"
run = "echo 2"
"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("more than once"), "{err}");
}

#[test]
fn flow_digest_ignores_declaration_order() {
    let reordered = r#"
[flow]
name = "demo"

[[step]]
id = "a"
run = "echo a"

[[step]]
id = "b"
run = "echo b"
needs = ["a"]
"#;
    assert_eq!(
        flow(BASIC).unwrap().digest(),
        flow(reordered).unwrap().digest()
    );
}

#[test]
fn flow_digest_tracks_command_changes() {
    let changed = BASIC.replace("echo b", "echo c");
    assert_ne!(
        flow(BASIC).unwrap().digest(),
        flow(&changed).unwrap().digest()
    );
}

#[test]
fn only_plain_relative_paths_are_contained() {
    for path in ["a.txt", "build/out.txt", "a/b/c"] {
        assert!(is_contained(path), "{path}");
    }
    for path in ["", "../a", "a/../../b", "/etc/passwd", "./a"] {
        assert!(!is_contained(path), "{path}");
    }
}

#[test]
fn a_step_declares_only_files_its_patterns_match() {
    let text = r#"
[flow]
name = "o"

[[step]]
id = "s"
run = "true"
outputs = ["build/*.txt", "dist/**/*.js", "REPORT.md"]
"#;
    let flow = flow(text).unwrap();
    let step = flow.step("s").unwrap();
    for path in ["build/a.txt", "dist/x.js", "dist/a/b/x.js", "REPORT.md"] {
        assert!(step.declares_output(path), "{path}");
    }
    for path in [
        "build/sub/a.txt",
        "build/a.md",
        "other.txt",
        "../REPORT.md",
        "dist/../REPORT.md",
    ] {
        assert!(!step.declares_output(path), "{path}");
    }
}

#[test]
fn closure_keeps_only_transitive_dependencies() {
    let text = r#"
[flow]
name = "demo"

[[step]]
id = "a"
run = "echo a"

[[step]]
id = "b"
run = "echo b"
needs = ["a"]

[[step]]
id = "side"
run = "echo side"
"#;
    let flow = flow(text).unwrap();
    assert_eq!(flow.closure("b").unwrap(), vec!["a", "b"]);
}

#[test]
fn budget_falls_back_to_flow_then_default() {
    let text = r#"
[flow]
name = "demo"
budget = { timeout_secs = 5 }

[[step]]
id = "a"
run = "echo a"

[[step]]
id = "b"
run = "echo b"
budget = { timeout_secs = 9, max_inline_bytes = 16 }
"#;
    let flow = flow(text).unwrap();
    let a = flow.resolved_budget(flow.step("a").unwrap());
    assert_eq!(a.timeout_secs, 5);
    assert_eq!(a.max_inline_bytes, DEFAULT_MAX_INLINE_BYTES);
    let b = flow.resolved_budget(flow.step("b").unwrap());
    assert_eq!(b.timeout_secs, 9);
    assert_eq!(b.max_inline_bytes, 16);
}

#[test]
fn reports_claims_that_have_no_verify_command() {
    let text = r#"
[flow]
name = "demo"

[[step]]
id = "proven"
run = "echo a"
claims = ["a holds"]
verify = "true"

[[step]]
id = "unproven"
run = "echo b"
claims = ["b holds"]
"#;
    let flow = flow(text).unwrap();
    let ids: Vec<&str> = flow
        .unproven_claim_steps()
        .iter()
        .map(|step| step.id.as_str())
        .collect();
    assert_eq!(ids, vec!["unproven"]);
}
