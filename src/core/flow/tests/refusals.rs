//! Tests of what a flow file is refused for: a proof setting without the
//! proof it needs or with paths it cannot use, and a key flot does not know.

use crate::core::flow::Mutants;

use super::flow;

#[test]
fn fails_before_needs_a_proof_and_no_candidates() {
    let base =
        "[flow]\nname = \"demo\"\n\n[[step]]\nid = \"a\"\nrun = \"true\"\nfails_before = true\n";
    let proven = format!("{base}claims = [\"x\"]\nverify = \"true\"\n");
    assert!(flow(&proven).is_ok());
    for (extra, refusal) in [
        ("", "needs claims and a verify command"),
        ("claims = [\"x\"]\n", "needs claims and a verify command"),
        ("verify = \"true\"\n", "needs claims and a verify command"),
        (
            "claims = [\"x\"]\nverify = \"true\"\ncandidates = [\"p\", \"q\"]\n",
            "candidates do not support",
        ),
    ] {
        let error = format!("{:#}", flow(&format!("{base}{extra}")).unwrap_err());
        assert!(error.contains(refusal), "{extra}: {error}");
    }
    // Unset, it leaves the flow's digest as it was.
    let plain = flow(&proven.replace("fails_before = true\n", "")).unwrap();
    assert!(!serde_json::to_string(&plain.steps[0])
        .unwrap()
        .contains("fails_before"));
}

#[test]
fn mutants_need_a_proof_a_report_inside_its_directory_and_an_allow_list_in_the_root() {
    let base = "[flow]\nname = \"demo\"\n\n[[step]]\nid = \"a\"\nrun = \"true\"\n";
    let proven = format!("{base}claims = [\"x\"]\nverify = \"true\"\n");
    let mutants = "mutants = { run = \"m\", report = \"r.json\", allow = \"allow.toml\" }\n";
    let step = &flow(&format!("{proven}{mutants}")).unwrap().steps[0];
    assert_eq!(
        step.mutants,
        Some(Mutants {
            run: "m".into(),
            report: "r.json".into(),
            allow: Some("allow.toml".into()),
        })
    );
    // The allow list is read by the proof, once.
    assert_eq!(step.proof_inputs(), ["allow.toml"]);
    let listed = flow(&format!(
        "{proven}verify_inputs = [\"allow.toml\", \"t\"]\n{mutants}"
    ))
    .unwrap();
    assert_eq!(listed.steps[0].proof_inputs(), ["allow.toml", "t"]);
    for (text, refusal) in [
        (
            format!("{base}{mutants}"),
            "needs claims and a verify command",
        ),
        (
            format!("{base}claims = [\"x\"]\n{mutants}"),
            "needs claims and a verify command",
        ),
        (
            format!("{proven}{mutants}candidates = [\"p\", \"q\"]\n"),
            "candidates do not support",
        ),
        (
            format!("{proven}mutants = {{ run = \" \", report = \"r\" }}\n"),
            "empty mutants run command",
        ),
        (
            format!("{proven}mutants = {{ run = \"m\", report = \"../r\" }}\n"),
            "inside $FLOT_MUTANTS_DIR",
        ),
        (
            format!("{proven}mutants = {{ run = \"m\", report = \"r\", allow = \"a/*\" }}\n"),
            "without wildcards",
        ),
        (
            format!("{proven}outputs = [\"allow.toml\"]\n{mutants}"),
            "cannot produce its own proof",
        ),
        (
            format!("{proven}mutants = {{ run = \"m\" }}\n"),
            "missing field `report`",
        ),
    ] {
        let error = format!("{:#}", flow(&text).unwrap_err());
        assert!(error.contains(refusal), "{text}: {error}");
    }
    // Unset, it leaves the flow's digest as it was.
    assert!(!serde_json::to_string(&flow(&proven).unwrap().steps[0])
        .unwrap()
        .contains("mutants"));
}

#[test]
fn held_files_need_a_proof_paths_inside_the_root_and_no_candidates() {
    let base = "[flow]\nname = \"demo\"\n\n[[step]]\nid = \"a\"\nrun = \"true\"\n";
    let proven = format!("{base}claims = [\"x\"]\nverify = \"true\"\n");
    let flow_of = |extra: &str| flow(&format!("{proven}{extra}"));
    assert_eq!(
        flow_of("held = [\"tests/**\"]\n").unwrap().steps[0].held,
        ["tests/**"]
    );
    for (text, refusal) in [
        (
            format!("{base}held = [\"t\"]\n"),
            "needs claims and a verify command",
        ),
        (
            format!("{base}claims = [\"x\"]\nheld = [\"t\"]\n"),
            "needs claims and a verify command",
        ),
        (
            format!("{proven}held = [\"t\"]\ncandidates = [\"p\", \"q\"]\n"),
            "candidates do not support",
        ),
        (
            format!("{proven}held = [\"../t\"]\n"),
            "inside the flow root",
        ),
        (format!("{proven}held = [\"[\"]\n"), "not a valid glob"),
    ] {
        let error = format!("{:#}", flow(&text).unwrap_err());
        assert!(error.contains(refusal), "{text}: {error}");
    }
    // Unset, it leaves the flow's digest as it was.
    assert!(!serde_json::to_string(&flow(&proven).unwrap().steps[0])
        .unwrap()
        .contains("held"));
}

#[test]
fn a_misspelled_key_is_refused_not_ignored() {
    let base = "[flow]\nname = \"demo\"\n\n[[step]]\nid = \"a\"\nrun = \"true\"\n";
    assert!(flow(base).is_ok());
    for (extra, key) in [
        ("verfy = \"false\"\n", "verfy"),
        ("input = [\"src/**\"]\n", "input"),
        ("budget = { timeout = 5 }\n", "timeout"),
    ] {
        let error = format!("{:#}", flow(&format!("{base}{extra}")).unwrap_err());
        assert!(error.contains(key), "{key}: {error}");
    }
    let meta = "[flow]\nname = \"demo\"\nmax_paralel = 2\n\n[[step]]\nid = \"a\"\nrun = \"true\"\n";
    assert!(format!("{:#}", flow(meta).unwrap_err()).contains("max_paralel"));
    let top = format!("{base}\n[steps]\nid = \"b\"\n");
    assert!(format!("{:#}", flow(&top).unwrap_err()).contains("steps"));
}
