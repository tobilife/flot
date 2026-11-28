//! Tests of scans.

use super::*;
use crate::core::digest;

/// Over this repository's own files, enough for several threads, the
/// files read at once get the digests, bits and order that reading them
/// one at a time gives. The bits are the file system's, as a snapshot
/// reads them, not the ones git records.
#[test]
fn reading_files_at_once_records_what_reading_them_in_turn_does() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let patterns = ["src/**/*.rs".to_string(), "tests/**/*".to_string()];
    let scan = |keep| scan_in(root, None, &patterns, "input", Reach::Tracked, false, keep);
    let digests = Digests::default();
    let at_once = scan(Keep::<std::convert::Infallible>::Digest(&digests)).unwrap();
    let mut one = |bytes: &[u8]| Ok(digest::bytes(bytes));
    let in_turn = scan(Keep::With(&mut one)).unwrap();
    assert!(at_once.files.len() >= 64, "{} files", at_once.files.len());
    assert_eq!(at_once.files, in_turn.files);
    assert!(at_once.problems.is_empty() && at_once.unmatched.is_empty());
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn paths(scan: &Scan) -> Vec<&str> {
    scan.files.iter().map(|file| file.path.as_str()).collect()
}

/// The store is flot's own bookkeeping, rewritten by every run: no pattern
/// reaches into it, even one that names it.
#[test]
fn no_pattern_reaches_into_the_store_even_one_that_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let (root, store) = (dir.path(), crate::store::STORE_DIR);
    write(&root.join(store).join("runs").join("receipt.json"), "{}");
    write(&root.join("a.txt"), "a");
    let patterns = [
        format!("{store}/**/*"),
        format!("{store}/runs/receipt.json"),
        "**/*".to_string(),
    ];
    let scan = scan_globs(root, &patterns, "output", Reach::Everything, |bytes| {
        Ok::<_, std::convert::Infallible>(digest::bytes(bytes))
    })
    .unwrap_or_else(|never| match never {});
    assert_eq!(paths(&scan), ["a.txt"]);
    assert_eq!(scan.unmatched, &patterns[..2]);
}

/// An output reached through a symbolic link is not recorded. A pattern that
/// matched only such outputs is reported as matching nothing, once; one that
/// still matches a recorded file is not.
#[cfg(unix)]
#[test]
fn an_output_reached_through_a_link_is_left_out_and_its_pattern_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (root, outside) = (dir.path().join("work"), dir.path().join("outside"));
    write(&root.join("out.txt"), "out");
    write(&outside.join("secret.txt"), "secret");
    std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
    let patterns = [
        "*.txt".to_string(),
        "linked/*".to_string(),
        "missing/*".to_string(),
    ];
    let scan = scan_outputs(&root, &patterns, |bytes| Ok(digest::bytes(bytes))).unwrap();
    assert_eq!(paths(&scan), ["out.txt"]);
    assert_eq!(scan.unmatched, ["missing/*", "linked/*"]);
    assert_eq!(
        scan.problems,
        ["output linked/secret.txt is reached through a symbolic link and was not recorded"]
    );
}

/// A copy lives in the store, whose `.gitignore` ignores everything, and has
/// no `.git`: its snapshot is read with the project's rules, and holds what
/// the project's snapshot holds.
#[test]
fn a_snapshot_of_a_copy_holds_what_one_of_the_project_holds() {
    let dir = tempfile::tempdir().unwrap();
    let (project, store) = (dir.path().join("project"), dir.path().join("store"));
    let copy = store.join("copy");
    write(&store.join(".gitignore"), "*\n");
    write(
        &project.join(".git").join("info").join("exclude"),
        "*.local\n",
    );
    for root in [&project, &copy] {
        write(&root.join(".gitignore"), "build/\n");
        write(&root.join("src").join("lib.rs"), "lib\n");
        write(&root.join("notes.local"), "mine\n");
        write(&root.join("build").join("out.o"), "built\n");
    }
    let digests = Digests::default();
    let of_project = snapshot(&project, &project, &digests);
    assert_eq!(paths(&of_project), [".gitignore", "src/lib.rs"]);
    assert_eq!(snapshot(&copy, &project, &digests).files, of_project.files);
}

/// A snapshot keeps the executable bit the file system shows, even where git
/// does not trust it and a step's inputs take the bit git records.
#[cfg(unix)]
#[test]
fn a_snapshot_keeps_the_executable_bit_the_file_system_shows() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let run = root.join("run.sh");
    write(&run, "make\n");
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
    for args in [
        &["init", "-q"][..],
        &["config", "core.fileMode", "false"],
        &["add", "run.sh"],
    ] {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }
    let digests = Digests::default();
    let bits = |files: &[FileDigest]| -> Vec<(String, bool)> {
        files
            .iter()
            .map(|file| (file.path.clone(), file.executable))
            .collect()
    };
    let inputs = super::super::facts_of(
        root,
        &super::super::tests::step("inputs = [\"run.sh\"]"),
        &digests,
    )
    .inputs;
    assert_eq!(bits(&inputs), [("run.sh".to_string(), false)]);
    assert_eq!(
        bits(&snapshot(root, root, &digests).files),
        [("run.sh".to_string(), true)]
    );
}

#[test]
fn a_thread_reads_eight_files_or_more_and_no_more_threads_than_cores() {
    use super::threads_for;
    assert_eq!(threads_for(0, 16), 1);
    assert_eq!(threads_for(15, 16), 1);
    assert_eq!(threads_for(16, 16), 2);
    assert_eq!(threads_for(1000, 16), 8);
    assert_eq!(threads_for(1000, 3), 3);
}
