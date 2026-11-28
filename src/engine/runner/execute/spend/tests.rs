//! Reading back the file a command reports its spend in: up to 64 KiB of
//! it, and nothing but a regular file, said so when it is not one.

use super::read_metrics;

/// A metrics file of `len` bytes: a cost, padded with spaces.
fn padded(len: usize) -> String {
    let mut text = String::from(r#"{"cost_usd": 0.5}"#);
    text.push_str(&" ".repeat(len - text.len()));
    text
}

#[test]
fn a_metrics_file_of_64_kib_is_read_and_a_larger_one_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metrics.json");
    std::fs::write(&path, padded(64 * 1024)).unwrap();
    let (metrics, notes) = read_metrics(&path);
    assert_eq!(
        metrics.and_then(|metrics| metrics.cost_micro_usd),
        Some(500_000)
    );
    assert!(notes.is_empty(), "{notes:?}");
    assert!(!path.exists());

    std::fs::write(&path, padded(64 * 1024 + 1)).unwrap();
    let (metrics, notes) = read_metrics(&path);
    assert_eq!(metrics, None);
    assert_eq!(
        notes,
        ["metrics file ignored: it is larger than 65536 bytes; this step's spend is unknown"]
    );
    assert!(!path.exists());
}

/// No file is a step that reported nothing; a path that cannot even be
/// inspected, here one through a file, is a report that cannot be read.
/// Unix only: Windows reads a path through a file as not found.
#[cfg(unix)]
#[test]
fn a_metrics_path_that_cannot_be_inspected_is_reported_and_a_missing_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        read_metrics(&dir.path().join("metrics.json")),
        (None, Vec::new())
    );
    let file = dir.path().join("file");
    std::fs::write(&file, "").unwrap();
    let (metrics, notes) = read_metrics(&file.join("metrics.json"));
    assert_eq!(metrics, None);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].starts_with("metrics file ignored: cannot inspect it ("),
        "{notes:?}"
    );
    assert!(
        notes[0].ends_with("; this step's spend is unknown"),
        "{notes:?}"
    );
}
