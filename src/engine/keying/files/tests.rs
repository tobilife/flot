//! Tests of one file at a time: what flot's patterns match, where a path
//! leads, and what a path holds.

use super::*;

/// `*` and `?` stay within a directory; only `**` crosses into others.
#[test]
fn a_wildcard_stays_within_a_directory_unless_it_is_a_double_star() {
    let matches = |pattern: &str, path: &str| {
        glob::Pattern::new(pattern)
            .unwrap()
            .matches_with(path, match_options())
    };
    assert!(matches("src/*.rs", "src/lib.rs"));
    assert!(!matches("src/*.rs", "src/engine/keying.rs"));
    assert!(!matches("*.rs", "src/lib.rs"));
    assert!(!matches("src?lib.rs", "src/lib.rs"));
    assert!(matches("src/**/*.rs", "src/engine/keying.rs"));
}

/// A file's content is its digest. A directory, which no recorded file is,
/// holds an empty one, and nothing there holds none.
#[test]
fn the_content_of_a_directory_is_an_empty_digest() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src").join("lib.rs"), "lib").unwrap();
    assert_eq!(
        content_of(dir.path(), "src/lib.rs").unwrap(),
        Some(digest::bytes(b"lib"))
    );
    assert_eq!(content_of(dir.path(), "src").unwrap(), Some(String::new()));
    assert_eq!(content_of(dir.path(), "src/gone.rs").unwrap(), None);
}

/// A path that cannot be looked at, such as one through a file, neither
/// stays within the root nor holds nothing: what it is is not known, unlike
/// a path that is not there yet.
#[cfg(unix)]
#[test]
fn a_path_through_a_file_is_an_error_where_a_missing_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.txt"), "a").unwrap();
    assert!(within_root(root, "missing/b.txt").unwrap());
    assert_eq!(content_of(root, "missing/b.txt").unwrap(), None);

    let error = within_root(root, "a.txt/b.txt").unwrap_err();
    assert!(error.to_string().contains("cannot inspect"), "{error:#}");
    let error = content_of(root, "a.txt/b.txt").unwrap_err();
    assert!(
        error.to_string().contains("cannot inspect a.txt/b.txt"),
        "{error:#}"
    );
}
