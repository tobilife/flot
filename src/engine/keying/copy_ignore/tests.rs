//! Tests of the project's ignore rules applied to a copy of it.

use super::*;

/// A project in `work/project`, and a copy of it in a store whose
/// `.gitignore` ignores everything, as flot's does.
fn layout() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("work").join("project");
    let copy = dir.path().join("store").join("copy");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&copy).unwrap();
    write(&dir.path().join("store").join(".gitignore"), "*\n");
    (dir, project, copy)
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The copy's own `.gitignore` files are read from the copy, the nearest
/// first; the store's, around the copy, is not read at all.
#[test]
fn a_copy_s_own_gitignore_files_decide_nearest_first() {
    let (_dir, project, copy) = layout();
    write(&copy.join(".gitignore"), "build/\n*.log\n");
    write(&copy.join("logs").join(".gitignore"), "!keep.log\n");
    let rules = CopyIgnore::new(&copy, &project);
    assert!(rules.ignores(&copy.join("build"), true));
    assert!(rules.ignores(&copy.join("logs").join("debug.log"), false));
    assert!(!rules.ignores(&copy.join("logs").join("keep.log"), false));
    assert!(!rules.ignores(&copy.join("src"), true));
    assert!(!rules.ignores(&copy.join("src").join("lib.rs"), false));
}

/// What lies above the project is read where the project is, not above the
/// copy: the `.gitignore` of a directory above it, and `info/exclude` of
/// the repository it is in. The copy's own rules still come first.
#[test]
fn the_rules_above_the_project_apply_to_its_copy() {
    let (dir, project, copy) = layout();
    let work = dir.path().join("work");
    write(&work.join(".gitignore"), "*.bak\n");
    write(&work.join(".git").join("info").join("exclude"), "*.tmp\n");
    write(&copy.join(".gitignore"), "!keep.bak\n");
    let rules = CopyIgnore::new(&copy, &project);
    assert!(rules.ignores(&copy.join("old.bak"), false));
    assert!(rules.ignores(&copy.join("src").join("scratch.tmp"), false));
    assert!(!rules.ignores(&copy.join("keep.bak"), false));
    assert!(!rules.ignores(&copy.join("src").join("main.rs"), false));
}

/// A copy has no `.git`: `info/exclude` of the project's repository, and of
/// one nested in it, are read from the project, each for its own tree, after
/// the copy's `.gitignore` files.
#[test]
fn the_exclude_files_of_the_project_s_repositories_apply_to_its_copy() {
    let (_dir, project, copy) = layout();
    write(
        &project.join(".git").join("info").join("exclude"),
        "*.local\n",
    );
    let vendor = project.join("vendor");
    write(
        &vendor.join(".git").join("info").join("exclude"),
        "gen.rs\n",
    );
    write(&copy.join(".gitignore"), "!kept.local\n");
    let rules = CopyIgnore::new(&copy, &project);
    assert!(rules.ignores(&copy.join("notes.local"), false));
    assert!(rules.ignores(&copy.join("vendor").join("gen.rs"), false));
    assert!(!rules.ignores(&copy.join("gen.rs"), false));
    assert!(!rules.ignores(&copy.join("kept.local"), false));
}

/// In a linked work tree `.git` is a file naming its own git directory, and
/// `info/exclude` is the one of the repository the work tree belongs to.
#[test]
fn a_linked_work_tree_s_exclude_file_is_its_repository_s() {
    let (dir, project, copy) = layout();
    let main = dir.path().join("main").join(".git");
    let linked = main.join("worktrees").join("project");
    write(&main.join("info").join("exclude"), "*.local\n");
    write(&linked.join("commondir"), "../..\n");
    write(
        &project.join(".git"),
        &format!("gitdir: {}\n", linked.display()),
    );
    let rules = CopyIgnore::new(&copy, &project);
    assert!(rules.ignores(&copy.join("notes.local"), false));
    assert!(!rules.ignores(&copy.join("notes.txt"), false));
}
