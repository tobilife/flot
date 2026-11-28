//! Tests of the copies of the project a step runs in.

use super::super::tests::step;
use super::*;

#[test]
fn a_copy_of_the_work_is_written_when_it_is_made() {
    let dir = tempfile::tempdir().unwrap();
    let (root, dest) = (dir.path().join("work"), dir.path().join("copy"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("old.rs"), "fn old() {}").unwrap();
    let long_ago =
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(root.join("old.rs"))
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    let mut readonly = std::fs::metadata(root.join("old.rs"))
        .unwrap()
        .permissions();
    readonly.set_readonly(true);
    std::fs::set_permissions(root.join("old.rs"), readonly).unwrap();

    let before = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
    copy_workspace(&root, &dest, &step("")).unwrap();

    let copied = std::fs::metadata(dest.join("old.rs")).unwrap();
    assert_eq!(std::fs::read(dest.join("old.rs")).unwrap(), b"fn old() {}");
    assert!(copied.permissions().readonly());
    // A build that goes by times must see it as newer than anything it built.
    assert!(
        copied.modified().unwrap() > before,
        "{:?}",
        copied.modified()
    );
}

#[test]
fn the_kept_copy_is_brought_to_the_work_writing_only_what_changed() {
    let dir = tempfile::tempdir().unwrap();
    let (root, dest) = (dir.path().join("work"), dir.path().join("copy"));
    let write = |path: &Path, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(&root.join("src/lib.rs"), "lib\n");
    write(&root.join("src/gone.rs"), "gone\n");
    write(&root.join("tests/t.rs"), "test\n");
    let kept = dir.path().join("copy.json");
    let sync = |stamp: std::time::SystemTime| {
        let copied = Digests::kept(&kept, dir.path().join("fence"));
        let work = Digests::fenced_in(dir.path().join("fence"));
        let written = sync_workspace(&root, &dest, &step(""), &work, &copied, stamp).unwrap();
        copied.keep(&kept).unwrap();
        written
    };
    let modified = |path: &str| {
        std::fs::metadata(dest.join(path))
            .unwrap()
            .modified()
            .unwrap()
    };
    let first = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
    assert_eq!(sync(first), 3);
    assert_eq!(modified("src/lib.rs"), first);
    assert_eq!(sync(first + std::time::Duration::from_secs(10)), 0);
    assert_eq!(
        modified("src/lib.rs"),
        first,
        "an unchanged file is left as it is"
    );

    // The work changes; a proof left the copy otherwise.
    write(&root.join("src/lib.rs"), "lib, changed\n");
    std::fs::remove_file(root.join("src/gone.rs")).unwrap();
    write(&dest.join("tests/t.rs"), "TEST\n");
    write(&dest.join("junk/out.txt"), "junk\n");
    write(&dest.join("stray.txt"), "stray\n");
    #[cfg(unix)]
    {
        std::fs::remove_file(dest.join("src/lib.rs")).unwrap();
        std::os::unix::fs::symlink(root.join("src/lib.rs"), dest.join("src/lib.rs")).unwrap();
    }
    let second = first + std::time::Duration::from_secs(20);
    assert_eq!(sync(second), 2);
    for (path, text) in [("src/lib.rs", "lib, changed\n"), ("tests/t.rs", "test\n")] {
        assert_eq!(std::fs::read_to_string(dest.join(path)).unwrap(), text);
        assert_eq!(modified(path), second, "{path}");
        assert!(!std::fs::symlink_metadata(dest.join(path))
            .unwrap()
            .is_symlink());
    }
    for path in ["src/gone.rs", "junk", "stray.txt"] {
        assert!(!dest.join(path).exists(), "{path}");
    }

    // Changed in place, its size and modification time as they were.
    std::fs::write(dest.join("tests/t.rs"), "tset\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(dest.join("tests/t.rs"))
        .unwrap()
        .set_modified(second)
        .unwrap();
    assert_eq!(sync(second), 1);
    assert_eq!(
        std::fs::read_to_string(dest.join("tests/t.rs")).unwrap(),
        "test\n"
    );

    // A file the proof replaced with a directory, and the other way round.
    std::fs::remove_file(dest.join("tests/t.rs")).unwrap();
    std::fs::create_dir_all(dest.join("tests/t.rs/inner")).unwrap();
    std::fs::remove_dir_all(dest.join("src")).unwrap();
    write(&dest.join("src"), "not a directory\n");
    assert_eq!(sync(second), 2);
    assert_eq!(
        std::fs::read_to_string(dest.join("tests/t.rs")).unwrap(),
        "test\n"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("src/lib.rs")).unwrap(),
        "lib, changed\n"
    );
}
