//! Declared outputs: put back on reuse, never followed through a link.

use crate::support::{read, Project};

const PRODUCES: &str = r#"
[flow]
name = "produces"

[[step]]
id = "make"
run = "mkdir -p build && tr a-z A-Z < in.txt > build/out.txt"
inputs = ["in.txt"]
outputs = ["build/*.txt"]
claims = ["the output is the input in upper case"]
verify = "grep -qx HELLO build/out.txt"
"#;

#[test]
fn a_reused_receipt_puts_back_the_files_it_proved() {
    let project = Project::new(PRODUCES);
    project.write("in.txt", "hello\n");
    project.flot(&["run"]).ok().has("> build/out.txt");

    // Collection keeps output objects: they are evidence of a recorded run.
    project.flot(&["gc"]).ok();
    std::fs::remove_dir_all(project.path().join("build")).unwrap();
    project
        .flot(&["run"])
        .ok()
        .has("cached")
        .has("output build/out.txt was missing and was restored from its receipt");
    assert_eq!(read(&project, "build/out.txt"), "HELLO\n");

    // Nothing to put back: no note.
    project.flot(&["run"]).ok().lacks("restored");
}

#[test]
fn a_changed_output_is_neither_overwritten_nor_vouched_for() {
    let project = Project::new(
        r#"
[flow]
name = "drift"

[[step]]
id = "make"
run = "test -f out.txt || echo generated > out.txt"
outputs = ["out.txt"]
claims = ["out.txt says generated"]
verify = "grep -qx generated out.txt"
"#,
    );
    project.flot(&["run"]).ok();
    project.write("out.txt", "hand edit\n");
    // The old receipt proved other content, so it is not reused, and the edit
    // is not overwritten: the step runs again and its proof judges the file.
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output
        .has("output out.txt changed since its receipt; the step runs again")
        .has("ran      make")
        .has("claims 0v/0u/1r");
    assert_eq!(read(&project, "out.txt"), "hand edit\n");
}

#[cfg(unix)]
#[test]
fn outputs_are_never_read_or_written_through_a_symbolic_link() {
    let outside = tempfile::tempdir().unwrap();
    let project = Project::new(PRODUCES);
    project.write("in.txt", "hello\n");
    project.flot(&["run"]).ok();

    // Swap the output directory for a link to somewhere else: reuse must not
    // write through it.
    std::fs::remove_dir_all(project.path().join("build")).unwrap();
    std::os::unix::fs::symlink(outside.path(), project.path().join("build")).unwrap();
    // The cached receipt is passed over rather than restored through the
    // link; the step runs, and what it wrote through the link is not recorded.
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output
        .has("output build/out.txt is reached through a symbolic link; the cached receipt is not reused")
        .has("ran      make")
        .lacks("was missing and was restored");

    // A link planted where an output is expected is never recorded.
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "do not export\n").unwrap();
    let leak = Project::new(
        "[flow]\nname = \"leak\"\n\n[[step]]\nid = \"a\"\nrun = \"true\"\noutputs = [\"*.txt\"]\n",
    );
    std::os::unix::fs::symlink(&secret, leak.path().join("leak.txt")).unwrap();
    let output = leak.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output
        .has("output leak.txt is reached through a symbolic link and was not recorded")
        .lacks("> leak.txt");
}

#[test]
fn outputs_travel_with_a_bundle() {
    let source = Project::new(PRODUCES);
    source.write("in.txt", "hello\n");
    source.flot(&["run"]).ok();
    let bundle = source.path().join("run.flotbundle.json");
    source
        .flot(&["bundle", "-o", bundle.to_str().unwrap()])
        .ok();

    let target = Project::new(PRODUCES);
    target.write("in.txt", "hello\n");
    // Unsigned evidence can be inspected, but it never stands in for work.
    target
        .flot(&["import", bundle.to_str().unwrap()])
        .ok()
        .has("not reusable");
    target.flot(&["run"]).ok().has("ran      make");
    assert_eq!(read(&target, "build/out.txt"), "HELLO\n");
}

#[test]
fn a_trusted_bundle_restores_the_files_it_proved() {
    let source = Project::new(PRODUCES);
    source.write("in.txt", "hello\n");
    source.flot(&["run"]).ok();
    let key = source
        .flot(&["key", "init"])
        .ok()
        .stdout
        .lines()
        .last()
        .unwrap()
        .to_string();
    let bundle = source.path().join("run.flotbundle.json");
    source
        .flot(&["bundle", "--sign", "-o", bundle.to_str().unwrap()])
        .ok();

    let target = Project::new(PRODUCES);
    target.write("in.txt", "hello\n");
    target.flot(&["trust", "add", "alice", &key]).ok();
    target
        .flot(&["import", bundle.to_str().unwrap()])
        .ok()
        .has("reusable: local runs may reuse");
    target
        .flot(&["run"])
        .ok()
        .has("cached")
        .has("was missing and was restored");
    assert_eq!(read(&target, "build/out.txt"), "HELLO\n");
}

#[test]
fn a_receipt_naming_an_undeclared_file_is_never_reused() {
    use flot::core::receipt::{FileDigest, Receipt};

    let project = Project::new(PRODUCES);
    project.write("in.txt", "hello\n");
    project.flot(&["run"]).ok();

    // Forge what a hostile bundle could import: an authentic receipt for the
    // same step key that also lists a file outside the step's outputs.
    let store = project.store();
    let run = store.resolve_run("latest").unwrap();
    let honest = store.load_receipt(&run.steps[0].receipt_id).unwrap();
    let mut identity = honest.identity.clone();
    identity.outputs.push(FileDigest {
        executable: false,
        path: "planted.sh".into(),
        digest: store.put_bytes(b"echo pwned\n").unwrap(),
    });
    let forged = Receipt::seal(identity, honest.observed.clone());
    store.put_receipt(&forged).unwrap();
    store
        .remember(&forged.identity.step_key, &forged.receipt_id, &[])
        .unwrap();

    std::fs::remove_dir_all(project.path().join("build")).unwrap();
    project.flot(&["run"]).ok().has("ran      make");
    assert!(!project.path().join("planted.sh").exists());
}
