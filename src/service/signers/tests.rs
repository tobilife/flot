//! Tests of the trust list: whom a name stands for as signers are added,
//! removed and follow their handovers.

use std::collections::BTreeMap;

use crate::core::attest::{hand_over, public_key_of};

use super::super::Workspace;

fn workspace() -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("flot.toml"),
        "[flow]\nname = \"t\"\n\n[[step]]\nid = \"a\"\nrun = \"true\"\n",
    )
    .unwrap();
    let workspace = Workspace::at(dir.path(), &dir.path().join("flot.toml"));
    (dir, workspace)
}

fn trusted(workspace: &Workspace) -> BTreeMap<String, String> {
    workspace.store().unwrap().trusted_keys().unwrap()
}

#[test]
fn readding_a_name_replaces_only_the_keys_it_stood_for() {
    let (_dir, workspace) = workspace();
    let (old, bob, new) = (
        public_key_of(&[1; 32]),
        public_key_of(&[2; 32]),
        public_key_of(&[3; 32]),
    );
    workspace.trust_add("alice", &old).unwrap();
    workspace.trust_add("bob", &bob).unwrap();
    workspace.trust_add("alice", &new).unwrap();
    assert_eq!(
        trusted(&workspace),
        BTreeMap::from([(bob, "bob".to_string()), (new, "alice".to_string())])
    );
}

#[test]
fn removing_a_name_removes_that_signer_and_no_other() {
    let (_dir, workspace) = workspace();
    let (alice, bob) = (public_key_of(&[1; 32]), public_key_of(&[2; 32]));
    workspace.trust_add("alice", &alice).unwrap();
    workspace.trust_add("bob", &bob).unwrap();
    let removed = workspace.trust_remove("alice").unwrap();
    assert_eq!((removed.name.as_str(), removed.key), ("alice", alice));
    assert_eq!(
        trusted(&workspace),
        BTreeMap::from([(bob, "bob".to_string())])
    );
}

#[test]
fn a_handover_between_two_signers_trusted_apart_is_refused() {
    let (dir, workspace) = workspace();
    let (old, new) = ([1; 32], [2; 32]);
    workspace.trust_add("alice", &public_key_of(&old)).unwrap();
    workspace.trust_add("bob", &public_key_of(&new)).unwrap();
    let path = dir.path().join("handover.json");
    std::fs::write(
        &path,
        serde_json::to_string(&hand_over(&old, &new)).unwrap(),
    )
    .unwrap();
    let before = trusted(&workspace);
    let error = workspace.trust_rotate(&path, false).unwrap_err();
    assert!(
        format!("{error:#}").contains("is already trusted as bob, not as alice"),
        "{error:#}"
    );
    assert_eq!(trusted(&workspace), before);

    // Once the old key alone is trusted, the handover carries its name to
    // the new key; followed again, both are that signer's and it holds.
    workspace.trust_remove("bob").unwrap();
    let rotation = workspace.trust_rotate(&path, false).unwrap();
    assert_eq!((rotation.name.as_str(), rotation.added), ("alice", true));
    let again = workspace.trust_rotate(&path, false).unwrap();
    assert_eq!((again.name.as_str(), again.added), ("alice", false));
}
