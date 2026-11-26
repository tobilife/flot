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
    /// The path component right after the layer, without `.rs`, so adapters
    /// can be told apart from each other: `adapters/cli.rs` and
    /// `adapters/cli/args.rs` both belong to `cli`.
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
        let unit = parts.next().map_or(layer.clone(), |second| {
            second.trim_end_matches(".rs").to_string()
        });
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

/// Every reference that does not point down the layers.
fn layer_violations(sources: &[Source]) -> Vec<String> {
    let mut violations = Vec::new();
    for source in sources {
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
    violations
}

#[test]
fn dependencies_only_point_down_the_layers() {
    let violations = layer_violations(&sources());
    assert!(violations.is_empty(), "\n{}", violations.join("\n"));
}

/// Every adapter file that names another adapter. `adapters/mod.rs` declares
/// them all, so it is the one file not checked.
fn cross_adapter_violations(sources: &[Source]) -> Vec<String> {
    let adapters: Vec<&Source> = sources
        .iter()
        .filter(|source| source.layer == "adapters" && source.unit != "mod")
        .collect();
    let units: BTreeSet<&str> = adapters.iter().map(|source| source.unit.as_str()).collect();
    let mut violations = Vec::new();
    for source in &adapters {
        for other in &units {
            if source.unit == *other {
                continue;
            }
            let needle = format!("adapters::{other}");
            if source.text.contains(&needle) || source.text.contains(&format!("super::{other}")) {
                violations.push(format!(
                    "{} reaches into the {other} adapter",
                    source.path.display()
                ));
            }
        }
    }
    violations
}

#[test]
fn adapters_do_not_know_about_each_other() {
    let violations = cross_adapter_violations(&sources());
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

#[test]
fn the_guard_itself_tells_adapters_apart_by_their_directory() {
    // An adapter split into a file and a directory beside it, as `cli` may
    // be, read the way the real tree is.
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let files = [
        ("adapters/mod.rs", "pub mod cli;\npub mod mcp;\n"),
        ("adapters/cli.rs", "mod args;\nmod hook;\nmod render;\n"),
        ("adapters/cli/args.rs", "use super::mcp::serve;\n"),
        ("adapters/cli/render.rs", "fn f() { crate::adapters::mcp::serve() }\n"),
        (
            "adapters/cli/hook.rs",
            "use super::args::Args;\nuse super::super::cli::render;\nuse crate::adapters::cli::Shell;\n",
        ),
        ("adapters/mcp.rs", "mod wire;\npub fn serve() {}\n"),
        ("adapters/mcp/wire.rs", "use super::super::cli::main;\n"),
    ];
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let mut found = Vec::new();
    walk(root, root, &mut found);
    let shown = |path: &str| path.split('/').collect::<PathBuf>().display().to_string();

    let mut units: Vec<(String, &str)> = found
        .iter()
        .map(|source| (source.path.display().to_string(), source.unit.as_str()))
        .collect();
    units.sort();
    let mut expected = [
        (shown("adapters/mod.rs"), "mod"),
        (shown("adapters/cli.rs"), "cli"),
        (shown("adapters/cli/args.rs"), "cli"),
        (shown("adapters/cli/render.rs"), "cli"),
        (shown("adapters/cli/hook.rs"), "cli"),
        (shown("adapters/mcp.rs"), "mcp"),
        (shown("adapters/mcp/wire.rs"), "mcp"),
    ];
    expected.sort();
    assert_eq!(units, expected);

    // A nested file that names another adapter is caught, by `super::` or
    // by `crate::adapters::`; one that names its own adapter is not.
    let mut crossings = cross_adapter_violations(&found);
    crossings.sort();
    assert_eq!(
        crossings,
        [
            format!(
                "{} reaches into the mcp adapter",
                shown("adapters/cli/args.rs")
            ),
            format!(
                "{} reaches into the mcp adapter",
                shown("adapters/cli/render.rs")
            ),
            format!(
                "{} reaches into the cli adapter",
                shown("adapters/mcp/wire.rs")
            ),
        ]
    );

    // A `crate::adapters` path is refused by the layering rule on its own,
    // whichever adapter it names: inside an adapter, its own files are
    // reached through `super::`.
    let mut layered = layer_violations(&found);
    layered.sort();
    assert_eq!(
        layered,
        [
            format!(
                "{} (adapters) depends on adapters",
                shown("adapters/cli/hook.rs")
            ),
            format!(
                "{} (adapters) depends on adapters",
                shown("adapters/cli/render.rs")
            ),
        ]
    );
}
