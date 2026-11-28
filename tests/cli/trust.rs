//! Trusting a signer and the bundles they sign.

use crate::support::{public_key, run_id, Project, PROVEN};

fn signed_bundle(source: &Project) -> std::path::PathBuf {
    source.write("seed.txt", "one");
    source.flot(&["run"]).ok();
    let path = source.path().join("signed.flotbundle.json");
    source
        .flot(&["bundle", "--sign", "-o", path.to_str().unwrap()])
        .ok()
        .has("signed by ed25519:");
    path
}

#[test]
fn key_init_is_idempotent_and_never_prints_the_secret() {
    let project = Project::new(PROVEN);
    let first = project.flot(&["key", "init"]);
    first.ok().has("created signing key");
    let secret = std::fs::read_to_string(project.key_path()).unwrap();
    first.lacks(secret.trim());
    let again = project.flot(&["key", "init"]);
    again.ok().has("using signing key").lacks(secret.trim());
    assert_eq!(std::fs::read_to_string(project.key_path()).unwrap(), secret);
    project.flot(&["key", "show"]).ok().lacks(secret.trim());
}

#[test]
fn trusting_a_signer_and_no_longer_trusting_it_name_it_and_its_key() {
    let project = Project::new(PROVEN);
    let key = public_key(&project);
    let added = project.flot(&["trust", "add", "alice", &key]);
    assert_eq!(added.ok().stdout, format!("trusting {key} as alice\n"));
    let removed = project.flot(&["trust", "remove", "alice"]);
    assert_eq!(
        removed.ok().stdout,
        format!("no longer trusting alice ({key})\n")
    );
}

#[test]
fn signing_without_a_key_explains_how_to_make_one() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    let output = project.flot(&["bundle", "--sign"]);
    assert_eq!(output.code, 3);
    output.has("flot key init");
}

#[test]
fn a_trusted_signer_is_named_on_import_and_on_every_later_verify() {
    let source = Project::new(PROVEN);
    let key = public_key(&source);
    let bundle = signed_bundle(&source);

    let target = Project::new(PROVEN);
    target.flot(&["trust", "add", "alice", &key]).ok();
    target
        .flot(&["import", "--require-trusted", bundle.to_str().unwrap()])
        .ok()
        .has("authorship: trusted")
        .has("trusted as alice");
    target
        .flot(&["verify", &run_id(&source)])
        .ok()
        .has("integrity: intact")
        .has("authorship: trusted");

    // Revoking trust changes the judgement, not the evidence.
    target.flot(&["trust", "remove", "alice"]).ok();
    target
        .flot(&["verify", &run_id(&source)])
        .ok()
        .has("authorship: untrusted")
        .has("not on the trust list");
}

#[test]
fn every_report_names_who_vouched_for_a_step_reused_from_a_signed_run() {
    let source = Project::new(PROVEN);
    let key = public_key(&source);
    let bundle = signed_bundle(&source);
    // A host reusing its own work names no one.
    source
        .flot(&["run"])
        .ok()
        .has("cached   write")
        .lacks("vouched for");
    let own = source.flot(&["show", "--json"]).json();
    assert!(own["steps"][0].get("vouched_by").is_none(), "{own}");
    source
        .flot(&["verify", "--markdown"])
        .ok()
        .has("| `write` | ok, reused |")
        .lacks("vouched for")
        .lacks("Reused from signed runs");

    // CI's run is unsigned; its reports still say whose word a step rests on.
    // Its tree holds what the step left, as a checkout of the work would: the
    // receipt answers for that file.
    let target = Project::new(PROVEN);
    target.write("seed.txt", "one");
    target.write("artifact.txt", "flot\n");
    target.flot(&["trust", "add", "alice", &key]).ok();
    target
        .flot(&["import", "--require-trusted", bundle.to_str().unwrap()])
        .ok();
    target
        .flot(&["run"])
        .ok()
        .has("cached   write")
        .has("vouched for by alice");
    let shown = target.flot(&["show", "--json"]).json();
    assert_eq!(shown["steps"][0]["vouched_by"][0], key.as_str(), "{shown}");
    let run = shown["run_id"].as_str().unwrap().to_string();
    target
        .flot(&["verify", "--markdown"])
        .ok()
        .has("- Authorship: unsigned\n- Reused from signed runs: 1 step(s), vouched for by alice\n")
        .has("| `write` | ok, reused, vouched for by alice |");
    let audit = target.flot(&["verify", "--json"]).json();
    assert_eq!(audit["vouched"][0]["step_id"], "write", "{audit}");
    assert_eq!(audit["vouched"][0]["vouched_by"][0]["trusted_as"], "alice");
    assert_eq!(audit["vouched"][0]["vouched_by"][0]["confirmed"], true);

    // Once alice is no longer trusted, the recorded run names her key as
    // such, and the next run executes the step instead of reusing it.
    target.flot(&["trust", "remove", "alice"]).ok();
    let revoked = format!("vouched for by {} (not trusted here)", &key[..24]);
    target.flot(&["show", &run]).ok().has(&revoked);
    target
        .flot(&["verify", &run])
        .ok()
        .has(&format!("  - write: {revoked}"));
    target.flot(&["run"]).ok().has("ran      write");
}

#[test]
fn a_signer_a_run_names_without_their_signature_is_not_confirmed() {
    let alice = Project::new(PROVEN);
    let key = public_key(&alice);
    let mallory = Project::new(PROVEN);
    mallory.write("seed.txt", "one");
    mallory.flot(&["run"]).ok();
    mallory.flot(&["run"]).ok().has("cached   write");
    let path = mallory.path().join("forged.flotbundle.json");
    // Mallory's own run, edited to say alice vouched for its step.
    let forge = |run: &str| {
        mallory
            .flot(&["bundle", run, "-o", path.to_str().unwrap()])
            .ok();
        let mut bundle: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        bundle["run"]["steps"][0]["vouched_by"] = serde_json::json!([key]);
        std::fs::write(&path, bundle.to_string()).unwrap();
    };
    let target = Project::new(PROVEN);
    target.flot(&["trust", "add", "alice", &key]).ok();

    // A step the run executed rests on no one's word.
    forge("latest~1");
    let refused = target.flot(&["import", path.to_str().unwrap()]);
    assert_eq!(refused.code, 1, "{}", refused.stdout);
    refused.has("names signers who vouched for it, but the step was not reused");

    // A reused one may name anyone; only alice's signature would confirm it.
    forge("latest");
    target.flot(&["import", path.to_str().unwrap()]).ok();
    let run = run_id(&mallory);
    let unconfirmed = format!("vouched for by {} (not confirmed here)", &key[..24]);
    target
        .flot(&["show", &run])
        .ok()
        .has(&unconfirmed)
        .lacks("vouched for by alice");
    target
        .flot(&["verify", &run])
        .ok()
        .has(&format!("  - write: {unconfirmed}"))
        .lacks("vouched for by alice");
    target
        .flot(&["verify", "--markdown", &run])
        .ok()
        .has(&format!(
            "| `write` | ok, reused, vouched for by {} \\(not confirmed here\\) |",
            &key[..24]
        ))
        .lacks("vouched for by alice");
    let audit = target.flot(&["verify", "--json", &run]).json();
    assert_eq!(audit["vouched"][0]["vouched_by"][0]["confirmed"], false);
    assert_eq!(
        audit["vouched"][0]["vouched_by"][0]["trusted_as"],
        serde_json::Value::Null
    );
}

#[test]
fn require_trusted_refuses_an_unknown_signer_and_leaves_nothing_behind() {
    let source = Project::new(PROVEN);
    public_key(&source);
    let bundle = signed_bundle(&source);

    let target = Project::new(PROVEN);
    let output = target.flot(&["import", "--require-trusted", bundle.to_str().unwrap()]);
    assert_eq!(output.code, 3);
    output.has("bundle is untrusted");
    target.flot(&["runs"]).ok().has("no runs recorded");
}

#[test]
fn editing_a_signed_bundle_is_detected_even_when_every_digest_still_matches() {
    let source = Project::new(PROVEN);
    let key = public_key(&source);
    let bundle = signed_bundle(&source);

    // Change only the run's recorded duration: receipts, objects and the root
    // all stay valid, so integrity alone would accept this.
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bundle).unwrap()).unwrap();
    let duration = value["run"]["duration_ms"].as_u64().unwrap();
    value["run"]["duration_ms"] = serde_json::json!(duration + 1);
    std::fs::write(&bundle, serde_json::to_string(&value).unwrap()).unwrap();

    let target = Project::new(PROVEN);
    target.flot(&["trust", "add", "alice", &key]).ok();
    let output = target.flot(&["import", bundle.to_str().unwrap()]);
    assert_eq!(output.code, 1);
    output.has("altered after signing");
}

#[test]
fn a_weak_or_malformed_key_cannot_be_trusted() {
    let project = Project::new(PROVEN);
    let identity = format!("ed25519:01{}", "00".repeat(31));
    let output = project.flot(&["trust", "add", "mallory", &identity]);
    assert_eq!(output.code, 3);
    output.has("weak");
    assert_eq!(
        project.flot(&["trust", "add", "x", "ed25519:nothex"]).code,
        3
    );
    project
        .flot(&["trust", "list"])
        .ok()
        .has("no trusted signers");
}
