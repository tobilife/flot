//! Runs signed where they run, and keys handed over to the next.

use crate::support::{public_key, receipt_file, Project, TWO_STEPS};

#[test]
fn a_run_signed_here_carries_its_signature_into_verify_and_bundles() {
    let project = Project::new(TWO_STEPS);
    let key = project.flot(&["key", "init", "--json"]).json()["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    let run = project.flot(&["run", "--sign", "--json"]);
    run.ok();
    assert_eq!(run.json()["signed"]["signer"], key.as_str());
    assert_eq!(run.json()["signed"]["added"], true);
    project.flot(&["sign"]).ok().has("already signed");

    // Not on the trust list yet: named as this host's own key, not trusted.
    let fingerprint = &key[..24];
    project
        .flot(&["verify"])
        .ok()
        .has("authorship: this host's key")
        .has(&format!(
            "~ {fingerprint}  this host's key, not on the trust list"
        ))
        .lacks("untrusted");
    let audit = project.flot(&["verify", "--json"]).json();
    assert_eq!(audit["authorship"], "this_host", "{audit}");
    assert_eq!(audit["signers"][0]["this_host"], true, "{audit}");
    assert_eq!(audit["signers"][0]["trusted_as"], serde_json::Value::Null);
    project
        .flot(&["verify", "--markdown"])
        .ok()
        .has("- Authorship: this host's key");

    // Without the key at hand, the same signature is just untrusted.
    let away = project.key_path().with_extension("away");
    std::fs::rename(project.key_path(), &away).unwrap();
    project
        .flot(&["verify"])
        .ok()
        .has("authorship: untrusted")
        .has(&format!("? {fingerprint}  not on the trust list"));
    std::fs::rename(&away, project.key_path()).unwrap();

    project.flot(&["trust", "add", "me", &key]).ok();
    let audit = project.flot(&["verify", "--json"]).json();
    assert_eq!(audit["authorship"], "trusted", "{audit}");

    let path = project.path().join("signed.json");
    project.flot(&["bundle", "-o", path.to_str().unwrap()]).ok();
    let bundle: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(bundle["attestations"][0]["key"], key.as_str());
}

#[test]
fn run_sign_without_a_key_runs_nothing() {
    let project = Project::new(TWO_STEPS);
    let output = project.flot(&["run", "--sign"]);
    assert_eq!(output.code, 3, "stderr:\n{}", output.stderr);
    output.has("no signing key");
    assert!(!project.path().join(".flot").exists());
}

#[test]
fn a_run_whose_evidence_was_altered_is_never_signed() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["key", "init"]).ok();
    project.flot(&["run"]).ok();
    let path = receipt_file(&project, 0);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replacen("echo first", "echo forged", 1)).unwrap();
    for args in [&["sign"][..], &["bundle", "--sign"][..]] {
        let output = project.flot(args);
        assert_eq!(output.code, 1, "{args:?} stderr:\n{}", output.stderr);
        output.has("will not sign it");
    }
    assert!(
        !project.path().join(".flot/attestations").exists()
            || std::fs::read_dir(project.path().join(".flot/attestations"))
                .unwrap()
                .next()
                .is_none()
    );
}

/// `project` signs a fresh run and bundles it; returns (run id, bundle path).
fn signed_run_bundle(project: &Project, name: &str) -> (String, std::path::PathBuf) {
    project.flot(&["run", "--force", "--sign"]).ok();
    let run_id = project.flot(&["show", "--json"]).json()["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let path = project.path().join(name);
    project
        .flot(&["bundle", &run_id, "-o", path.to_str().unwrap()])
        .ok();
    (run_id, path)
}

fn authorship(project: &Project, run_id: &str) -> String {
    project.flot(&["verify", "--json", run_id]).json()["authorship"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn readers_who_trust_a_key_follow_its_handover_to_the_next() {
    let alice = Project::new(TWO_STEPS);
    let reader = Project::new(TWO_STEPS);
    let old = alice.flot(&["key", "init", "--json"]).json()["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    let (old_run, old_bundle) = signed_run_bundle(&alice, "old.json");
    reader.flot(&["trust", "add", "alice", &old]).ok();
    reader.flot(&["import", old_bundle.to_str().unwrap()]).ok();
    assert_eq!(authorship(&reader, &old_run), "trusted");

    let handover = alice.path().join("handover.json");
    let rotated = alice.flot(&["key", "rotate", "--json", "-o", handover.to_str().unwrap()]);
    rotated.ok();
    let rotated = rotated.json();
    let new = rotated["new_key"].as_str().unwrap().to_string();
    assert_eq!(rotated["old_key"], old.as_str());
    assert_ne!(new, old);
    assert!(std::path::Path::new(rotated["retired"].as_str().unwrap()).exists());
    assert_eq!(
        alice.flot(&["key", "show", "--json"]).json()["public_key"],
        new.as_str()
    );

    let (new_run, new_bundle) = signed_run_bundle(&alice, "new.json");
    reader.flot(&["import", new_bundle.to_str().unwrap()]).ok();
    assert_eq!(authorship(&reader, &new_run), "untrusted");

    reader
        .flot(&["trust", "rotate", handover.to_str().unwrap()])
        .ok()
        .has(&format!("trusting {new} as alice"))
        .has("still trusting");
    assert_eq!(authorship(&reader, &new_run), "trusted");
    assert_eq!(authorship(&reader, &old_run), "trusted");
    reader
        .flot(&["trust", "rotate", handover.to_str().unwrap()])
        .ok()
        .has("already trusting");
    let ambiguous = reader.flot(&["trust", "remove", "alice"]);
    assert_eq!(ambiguous.code, 3, "stderr:\n{}", ambiguous.stderr);
    ambiguous.has("names 2 trusted keys");

    reader
        .flot(&["trust", "rotate", "--revoke", handover.to_str().unwrap()])
        .ok()
        .has("no longer trusting");
    assert_eq!(authorship(&reader, &new_run), "trusted");
    assert_eq!(authorship(&reader, &old_run), "untrusted");
}

#[test]
fn key_rotate_names_both_keys_where_the_old_one_is_kept_and_the_handover() {
    let project = Project::new(TWO_STEPS);
    let old = public_key(&project);
    let handover = project.path().join("handover.json");
    let rotated = project.flot(&["key", "rotate", "-o", handover.to_str().unwrap()]);
    rotated.ok();
    let new = project.flot(&["key", "show", "--json"]).json()["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    let key = project.key_path();
    let retired = format!(
        "{}.retired-{}",
        key.display(),
        &old["ed25519:".len()..][..16]
    );
    assert!(std::path::Path::new(&retired).exists(), "{retired}");
    assert_eq!(
        rotated.stdout,
        format!(
            "rotated signing key {}\nnew  {new}\nold  {old}\n     kept as {retired}\nhandover {}\nnext: send it to everyone who trusts the old key; they run `flot trust rotate <file>`\n",
            key.display(),
            handover.display()
        )
    );
}

#[test]
fn a_handover_redirected_to_another_key_or_from_a_stranger_changes_no_trust() {
    let alice = Project::new(TWO_STEPS);
    let mallory = Project::new(TWO_STEPS);
    let reader = Project::new(TWO_STEPS);
    let old = alice.flot(&["key", "init", "--json"]).json()["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    let theirs = mallory.flot(&["key", "init", "--json"]).json()["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    reader.flot(&["trust", "add", "alice", &old]).ok();
    let handover = alice.path().join("handover.json");
    alice
        .flot(&["key", "rotate", "-o", handover.to_str().unwrap()])
        .ok();
    let mut statement: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&handover).unwrap()).unwrap();
    statement["to"] = theirs.clone().into();
    let forged = reader.path().join("forged.json");
    std::fs::write(&forged, statement.to_string()).unwrap();
    let output = reader.flot(&["trust", "rotate", forged.to_str().unwrap()]);
    assert_eq!(output.code, 1, "stderr:\n{}", output.stderr);
    output.has("does not authenticate");

    // Mallory's own, valid handover vouches for nothing where Mallory is
    // not trusted.
    let own = mallory.path().join("handover.json");
    mallory
        .flot(&["key", "rotate", "-o", own.to_str().unwrap()])
        .ok();
    let output = reader.flot(&["trust", "rotate", own.to_str().unwrap()]);
    assert_eq!(output.code, 3, "stderr:\n{}", output.stderr);
    output.has("does not trust");

    let list = reader.flot(&["trust", "list", "--json"]).json();
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list[0]["key"], old.as_str());
}

#[test]
fn a_rotation_that_cannot_write_its_handover_keeps_the_old_key() {
    let project = Project::new(TWO_STEPS);
    let missing = project.flot(&["key", "rotate"]);
    assert_eq!(missing.code, 3, "stderr:\n{}", missing.stderr);
    missing.has("flot key init");

    let old = project.flot(&["key", "init", "--json"]).json()["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    let taken = project.path().join("taken.json");
    std::fs::write(&taken, "mine").unwrap();
    let output = project.flot(&["key", "rotate", "-o", taken.to_str().unwrap()]);
    assert_ne!(output.code, 0);
    output.has("cannot write the handover");
    assert_eq!(std::fs::read_to_string(&taken).unwrap(), "mine");
    assert_eq!(
        project.flot(&["key", "show", "--json"]).json()["public_key"],
        old.as_str()
    );
}
