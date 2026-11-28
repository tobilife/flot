//! Tests of the copies' build: one proof at a time, its pruning, and the
//! time given to files written in the copy.

use std::fs;

use super::super::tests::store;

#[test]
fn one_proof_at_a_time_builds_in_the_copies_build() {
    let (_dir, store) = store();
    let held = store.builds(&|| false).unwrap();
    assert_eq!(held.target(), store.root().join("builds/target"));
    assert_eq!(held.work(), store.root().join("builds/work"));
    // Kept with the copy, and removed with it.
    assert_eq!(held.digests(), store.root().join("builds/work.json"));
    // Another holder, in this process or another, waits until told to stop.
    let error = store.builds(&|| true).err().unwrap().to_string();
    assert!(error.contains("another proof built"), "{error}");
    let waited = std::time::Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(held);
        });
        store.builds(&|| false).unwrap();
    });
    assert!(waited.elapsed() >= std::time::Duration::from_millis(300));
}

#[test]
fn a_run_removes_builds_no_proof_will_use() {
    let (_dir, store) = store();
    let builds = store.root().join("builds");
    let used = |ago: u64| {
        fs::File::options()
            .write(true)
            .open(builds.join("lock"))
            .unwrap()
            .set_modified(
                std::time::SystemTime::now() - std::time::Duration::from_secs(ago * 86_400),
            )
            .unwrap();
    };
    let kept = ["target/debug", "work/src", "work.json", "fence"];
    let all_there = |there: bool| {
        for path in kept {
            assert_eq!(builds.join(path).exists(), there, "{path}");
        }
    };
    drop(store.builds(&|| false).unwrap());
    for path in kept {
        if path.contains('/') {
            fs::create_dir_all(builds.join(path)).unwrap();
        } else {
            fs::write(builds.join(path), "x").unwrap();
        }
    }
    fs::create_dir_all(builds.join("cli/debug")).unwrap();
    // A process another test starts holds the lock until it runs its
    // program.
    let prune = || {
        for _ in 0..100 {
            if store.prune_builds() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the lock stayed held");
    };

    // Another layout's build goes at once; one used last week stays.
    used(6);
    prune();
    assert!(!builds.join("cli").exists());
    all_there(true);

    // Held by a proof, even an old one stays.
    used(8);
    let held = store.builds(&|| false).unwrap();
    used(8);
    assert!(!store.prune_builds());
    all_there(true);
    drop(held);

    // Released, it was used just now.
    prune();
    all_there(true);
    used(8);
    prune();
    all_there(false);
    assert!(builds.join("lock").exists());
}

#[test]
fn a_file_written_in_the_copy_is_newer_than_the_last_build() {
    let (_dir, store) = store();
    drop(store.builds(&|| false).unwrap());
    let released = fs::metadata(store.root().join("builds/lock"))
        .unwrap()
        .modified()
        .unwrap();
    let builds = store.builds(&|| false).unwrap();
    assert!(builds.stamp() >= released + std::time::Duration::from_secs(2));
    // Long after, it is now.
    let lock = store.root().join("builds/lock");
    drop(builds);
    fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60))
        .unwrap();
    let before = std::time::SystemTime::now();
    let stamp = store.builds(&|| false).unwrap().stamp();
    assert!(stamp >= before && stamp < before + std::time::Duration::from_secs(2));
}
