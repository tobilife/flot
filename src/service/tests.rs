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
