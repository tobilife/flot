//! The layering rules, enforced.
//!
//! ```text
//! core      digest, metrics, receipt model, flow model   no IO, depends on nothing else
//! store     content-addressed persistence                 -> core
//! engine    keying, exec, claims, scheduler, runner       -> core, store
//! service   the one contract both adapters expose         -> core, store, engine
//! adapters  cli, mcp                                      -> core, service
//! ```
//!
//! Dependencies only point down. Adapters never import each other; the binary
//! (`src/main.rs`) is the composition root that chooses between them. A rule
//! that is only written down drifts, so these are checked on every test run.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const LAYERS: [&str; 5] = ["core", "store", "engine", "service", "adapters"];

fn allowed(from: &str) -> &'static [&'static str] {
    match from {
        "core" => &["core"],
        "store" => &["core", "store"],
        "engine" => &["core", "store", "engine"],
        "service" => &["core", "store", "engine", "service"],
        "adapters" => &["core", "service"],
        other => panic!("unknown layer `{other}`"),
    }
}

/// Standard-library paths that perform IO. `core` must stay pure so identity
/// derivation can never depend on the machine it runs on.
const IO_PATHS: [&str; 5] = [
    "std::fs",
    "std::process",
    "std::net",
    "std::thread",
    "std::env",
];

struct Source {
    path: PathBuf,
    layer: String,
    /// Adapter file stem, so adapters can be told apart from each other.
    unit: String,
    text: String,
}

fn sources() -> Vec<Source> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    walk(&src, &src, &mut found);
    assert!(
        !found.is_empty(),
        "no sources found under {}",
        src.display()
    );
    found
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<Source>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(root, &path, found);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap();
        let mut parts = relative
            .iter()
            .map(|part| part.to_string_lossy().to_string());
        let first = parts.next().unwrap();
        let layer = first.trim_end_matches(".rs").to_string();
        if layer == "lib" || layer == "main" {
            continue;
        }
        let unit = relative.file_stem().unwrap().to_string_lossy().to_string();
        found.push(Source {
            text: std::fs::read_to_string(&path).unwrap(),
            path: relative.to_path_buf(),
            layer,
            unit,
        });
    }
}

/// Top-level modules referenced through `crate::`.
fn crate_references(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (index, _) in text.match_indices("crate::") {
        let rest = &text[index + "crate::".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            found.insert(name);
        }
    }
    found
}

#[test]
fn every_source_file_belongs_to_a_known_layer() {
    for source in sources() {
        assert!(
            LAYERS.contains(&source.layer.as_str()),
            "{} is outside every layer; place it under one of {:?}",
            source.path.display(),
            LAYERS
        );
    }
}

#[test]
fn dependencies_only_point_down_the_layers() {
    let mut violations = Vec::new();
    for source in sources() {
        for target in crate_references(&source.text) {
            if !LAYERS.contains(&target.as_str()) {
                violations.push(format!(
                    "{} references crate::{target}, which is not a layer",
                    source.path.display()
                ));
            } else if !allowed(&source.layer).contains(&target.as_str()) {
                violations.push(format!(
                    "{} ({}) depends on {target}",
                    source.path.display(),
                    source.layer
                ));
            }
        }
    }
    assert!(violations.is_empty(), "\n{}", violations.join("\n"));
}

#[test]
fn adapters_do_not_know_about_each_other() {
    let sources = sources();
    let adapters: Vec<&Source> = sources
        .iter()
        .filter(|source| source.layer == "adapters" && source.unit != "mod")
        .collect();
    let mut violations = Vec::new();
    for source in &adapters {
        for other in &adapters {
            if source.unit == other.unit {
                continue;
            }
            let needle = format!("adapters::{}", other.unit);
            if source.text.contains(&needle)
                || source.text.contains(&format!("super::{}", other.unit))
            {
                violations.push(format!(
                    "{} reaches into the {} adapter",
                    source.path.display(),
                    other.unit
                ));
            }
        }
    }
    assert!(violations.is_empty(), "\n{}", violations.join("\n"));
}

#[test]
fn core_performs_no_io() {
    let mut violations = Vec::new();
    for source in sources().iter().filter(|source| source.layer == "core") {
        for io in IO_PATHS {
            if source.text.contains(io) {
                violations.push(format!("{} uses {io}", source.path.display()));
            }
        }
    }
    assert!(violations.is_empty(), "\n{}", violations.join("\n"));
}

#[test]
fn the_guard_itself_detects_upward_and_unknown_references() {
    let refs = crate_references(
        "use crate::service::Workspace; let x = crate::core::digest::bytes(b\"\");",
    );
    assert_eq!(
        refs,
        BTreeSet::from(["core".to_string(), "service".to_string()])
    );
    assert!(!allowed("engine").contains(&"service"));
    assert!(!allowed("adapters").contains(&"engine"));
    assert!(!allowed("core").contains(&"store"));
}
