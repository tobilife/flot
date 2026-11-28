//! Tests of where a flow and its store are found, and of the starter flow
//! `flot init` writes.

use super::*;
use std::fs;

#[test]
fn find_flow_root_walks_upward() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("a/b/c");
    fs::create_dir_all(&nested).unwrap();
    fs::write(dir.path().join("flot.toml"), "").unwrap();
    assert_eq!(
        find_flow_root(&nested, Path::new("flot.toml")),
        dir.path().to_path_buf()
    );
}

#[test]
fn find_flow_root_falls_back_to_the_start_directory() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        find_flow_root(dir.path(), Path::new("flot.toml")),
        dir.path().to_path_buf()
    );
}

#[test]
fn a_named_flow_file_has_its_store_beside_it_however_it_is_named() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("sub")).unwrap();
    let flow = dir.path().join("sub/custom.toml");
    fs::write(&flow, "").unwrap();
    for named in [flow.clone(), PathBuf::from("sub/custom.toml")] {
        let workspace = Workspace::locate(dir.path(), Some(&named)).unwrap();
        assert_eq!(workspace.root(), dir.path().join("sub"));
        assert_eq!(workspace.flow_path(), flow.as_path());
    }
    let missing = Workspace::locate(dir.path(), Some(Path::new("absent.toml")));
    assert!(missing.unwrap_err().to_string().contains("does not exist"));
}

#[test]
fn init_never_writes_above_the_directory_it_runs_in() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("flot.toml"), "# parent").unwrap();
    let below = dir.path().join("sub");
    fs::create_dir_all(&below).unwrap();
    let workspace = Workspace::for_init(&below, None);
    assert_eq!(workspace.flow_path(), below.join("flot.toml"));
    workspace.init(true).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("flot.toml")).unwrap(),
        "# parent"
    );
}

#[test]
fn init_refuses_to_overwrite_without_force() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::at(dir.path(), &dir.path().join("flot.toml"));
    workspace.init(false).unwrap();
    assert!(workspace.init(false).is_err());
    workspace.init(true).unwrap();
    assert!(workspace.flow().is_ok(), "the starter flow must parse");
}

/// A remote whose default branch git would read as an option is not one a
/// starter flow holds its proofs to: it says what to set instead.
#[test]
fn init_holds_proofs_only_to_a_revision_git_reads_as_one() {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir.path())
            .env("GIT_CONFIG_GLOBAL", dir.path().join("no-global-gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "base"]);
    let workspace = Workspace::at(dir.path(), &dir.path().join("flot.toml"));
    for (head, held) in [
        (
            "refs/remotes/origin/main",
            "\nproofs_from = \"origin/main\"\n",
        ),
        ("refs/heads/-x", "\n# proofs_from = \"origin/main\"\n"),
    ] {
        git(&["symbolic-ref", "refs/remotes/origin/HEAD", head]);
        workspace.init(true).unwrap();
        let flow = fs::read_to_string(workspace.flow_path()).unwrap();
        assert!(flow.contains(held), "{head}: {flow}");
        assert!(workspace.flow().is_ok(), "{head}: {flow}");
    }
}
