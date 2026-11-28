//! Tests of the host signing key: creating, loading and rotating it, and the
//! owner-only files that hold secrets.

use super::*;

#[test]
fn a_generated_key_loads_back_to_the_same_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/signing.key");
    let created = generate(&path).unwrap();
    assert_eq!(load(&path).unwrap().public_key(), created.public_key());
}

#[test]
fn two_generated_keys_differ() {
    let dir = tempfile::tempdir().unwrap();
    let a = generate(&dir.path().join("a.key")).unwrap();
    let b = generate(&dir.path().join("b.key")).unwrap();
    assert_ne!(a.public_key(), b.public_key());
}

#[test]
fn an_existing_key_is_never_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("signing.key");
    let original = generate(&path).unwrap().public_key();
    let error = generate(&path).err().expect("second generate must fail");
    assert!(
        format!("{error:#}").contains("will not replace"),
        "{error:#}"
    );
    assert_eq!(load(&path).unwrap().public_key(), original);
}

#[cfg(unix)]
#[test]
fn the_key_file_is_owner_only_and_a_shared_one_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("signing.key");
    generate(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let error = load(&path).err().expect("a shared key must be refused");
    assert!(
        error.to_string().contains("readable by other users"),
        "{error}"
    );
}

#[test]
fn a_missing_or_garbled_key_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("signing.key");
    assert!(load(&path)
        .err()
        .unwrap()
        .to_string()
        .contains("flot key init"));
    write_owner_only(&path, b"not hex").unwrap();
    assert!(load(&path)
        .err()
        .unwrap()
        .to_string()
        .contains("not a flot signing key"));
}

#[test]
fn a_rotation_changes_nothing_until_committed_then_keeps_the_old_secret() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("signing.key");
    let old = generate(&path).unwrap().public_key();
    let rotation = begin_rotation(&path).unwrap();
    let handover = rotation.handover().clone();
    let retired = rotation.retired_path();
    assert_eq!(load(&path).unwrap().public_key(), old);
    assert!(!retired.exists());

    let new = rotation.commit().unwrap();
    assert_eq!(
        attest::check_handover(&handover),
        Ok((old.clone(), new.public_key()))
    );
    assert_eq!(load(&path).unwrap().public_key(), new.public_key());
    assert_eq!(load(&retired).unwrap().public_key(), old);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let files = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(files, 2, "the new key and the retired one, nothing aside");
}

#[test]
fn a_rotation_that_cannot_keep_the_old_secret_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("signing.key");
    let old = generate(&path).unwrap().public_key();
    let rotation = begin_rotation(&path).unwrap();
    std::fs::write(rotation.retired_path(), b"in the way").unwrap();
    let error = rotation.commit().err().expect("commit must fail");
    assert!(
        format!("{error:#}").contains("nothing was changed"),
        "{error:#}"
    );
    assert_eq!(load(&path).unwrap().public_key(), old);
}

#[test]
fn concurrent_creators_never_see_a_torn_secret() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("host.secret");
    let data: Vec<Vec<u8>> = (0..16u8).map(|n| vec![b'a' + n; 4096]).collect();
    let results: Vec<std::io::Result<()>> = std::thread::scope(|scope| {
        let handles: Vec<_> = data
            .iter()
            .map(|bytes| scope.spawn(|| write_owner_only(&path, bytes)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });
    let winners = results.iter().filter(|result| result.is_ok()).count();
    assert_eq!(winners, 1);
    assert!(results.iter().flatten().count() == 1);
    for result in &results {
        if let Err(error) = result {
            assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        }
    }
    let content = std::fs::read(&path).unwrap();
    assert!(
        data.contains(&content),
        "the file is one writer's whole content"
    );
    let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(leftovers, 1, "no aside files are left behind");
}
