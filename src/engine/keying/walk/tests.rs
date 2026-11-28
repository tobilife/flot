//! Tests of which files a pattern names.

use super::super::scan_globs;
use super::*;

/// Patterns walked together in this repository find, each, what walking
/// for it alone finds: shared starts, nested starts, a literal path, a
/// pattern matching nothing and an invalid one.
#[test]
fn patterns_walked_together_find_what_each_finds_alone() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let patterns = [
        "**/*.rs",
        "**/*.toml",
        "src/**/*.rs",
        "src/engine/*.rs",
        "Cargo.toml",
        "**/*.nothing",
        "[",
    ];
    let together = candidates_of(root, &patterns, Reach::Tracked, None);
    assert_eq!(together.len(), patterns.len());
    for (pattern, found) in patterns.iter().zip(together) {
        assert_eq!(
            found,
            candidates(root, pattern, Reach::Tracked, None),
            "{pattern}"
        );
        if !["**/*.nothing", "["].contains(pattern) {
            assert!(!found.unwrap().is_empty(), "{pattern} found nothing");
        }
    }
}

fn matched(root: &Path, pattern: &str, reach: Reach) -> Vec<String> {
    let scan = scan_globs(root, &[pattern.to_string()], "input", reach, |_| {
        Ok::<_, std::convert::Infallible>(String::new())
    })
    .unwrap_or_else(|never| match never {});
    scan.files.into_iter().map(|file| file.path).collect()
}

#[test]
fn a_literal_base_stops_at_the_first_wildcard() {
    assert_eq!(literal_base("Cargo.toml"), ("", false));
    assert_eq!(literal_base("src/lib.rs"), ("src", false));
    assert_eq!(literal_base("src/**/*.rs"), ("src", true));
    assert_eq!(literal_base("a/b/c*/d"), ("a/b", true));
    assert_eq!(literal_base("**/*"), ("", true));
    assert_eq!(literal_base("src/[ab].rs"), ("src", true));
}

#[test]
fn a_wildcard_input_skips_what_git_ignores_unless_the_pattern_spells_it_out() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for path in [
        "src/lib.rs",
        "target/debug/gen.rs",
        ".git/hooks/x.rs",
        ".flot/tmp/y.rs",
    ] {
        std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        std::fs::write(root.join(path), "fn main() {}").unwrap();
    }
    std::fs::write(root.join(".gitignore"), "target/\n").unwrap();

    assert_eq!(matched(root, "**/*.rs", Reach::Tracked), ["src/lib.rs"]);
    // Named through its literal directories, an ignored tree is an input.
    assert_eq!(
        matched(root, "target/**/*.rs", Reach::Tracked),
        ["target/debug/gen.rs"]
    );
    assert_eq!(
        matched(root, "target/debug/gen.rs", Reach::Tracked),
        ["target/debug/gen.rs"]
    );
    // Products are often ignored build output; a wildcard reaches them,
    // but never git's own files or the store.
    assert_eq!(
        matched(root, "**/*.rs", Reach::Everything),
        ["src/lib.rs", "target/debug/gen.rs"]
    );
}
