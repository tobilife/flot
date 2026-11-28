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
//! No source file, test files included, is longer than 300 lines.
//!
//! Dependencies only point down. Adapters never import each other; the binary
//! (`src/main.rs`) is the composition root that chooses between them.
//! Adapters never read a command line: they hand the guard a call, and the
//! service reads it with `core::shell` and `core::powershell`. A rule that is
//! only written down drifts, so these are checked on every test run.

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

/// The `core` modules that read a command line, which no adapter may name.
const READERS: [&str; 2] = ["shell", "powershell"];

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

/// The `core` modules a source names: as `core::name`, and each entry of a
/// `core::{..}` group, nested groups included.
fn core_modules(text: &str) -> BTreeSet<String> {
    let name = |text: &str| -> String {
        text.trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect()
    };
    let mut found = BTreeSet::new();
    for (index, _) in text.match_indices("core::") {
        let rest = &text[index + "core::".len()..];
        let Some(group) = rest.strip_prefix('{') else {
            found.insert(name(rest));
            continue;
        };
        let mut depth = 0;
        let mut start = 0;
        for (at, c) in group.char_indices() {
            match c {
                '{' => depth += 1,
                '}' if depth == 0 => {
                    found.insert(name(&group[start..at]));
                    break;
                }
                '}' => depth -= 1,
                ',' if depth == 0 => {
                    found.insert(name(&group[start..at]));
                    start = at + 1;
                }
                _ => {}
            }
        }
    }
    found.remove("");
    found
}

/// Every adapter file that names a command-line reader.
fn reader_violations(sources: &[Source]) -> Vec<String> {
    let mut violations = Vec::new();
    for source in sources.iter().filter(|source| source.layer == "adapters") {
        for module in core_modules(&source.text) {
            if READERS.contains(&module.as_str()) {
                violations.push(format!(
                    "{} names core::{module}; an adapter hands the guard a call instead",
                    source.path.display()
                ));
            }
        }
    }
    violations
}

#[test]
fn adapters_do_not_read_command_lines() {
    let violations = reader_violations(&sources());
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
fn the_guard_itself_finds_a_reader_named_by_an_adapter() {
    let source = |path: &str, text: &str| Source {
        path: PathBuf::from(path),
        layer: path.split('/').next().unwrap().to_string(),
        unit: String::new(),
        text: text.to_string(),
    };
    let found = reader_violations(&[
        source("adapters/cli.rs", "use crate::core::{powershell, shell};\n"),
        source(
            "adapters/cli/hook.rs",
            "let read = crate::core::shell::read(line);\n",
        ),
        source(
            "adapters/mcp.rs",
            "use crate::core::{receipt::{Receipt, Verdict}, shell::Origin};\n",
        ),
        source(
            "adapters/cli/render.rs",
            "use crate::core::digest;\nuse crate::service::{judge, Shell};\n",
        ),
        source(
            "service/guard/judge.rs",
            "use crate::core::{powershell, shell};\n",
        ),
    ]);
    let said = |path: &str, module: &str| {
        format!("{path} names core::{module}; an adapter hands the guard a call instead")
    };
    assert_eq!(
        found,
        [
            said("adapters/cli.rs", "powershell"),
            said("adapters/cli.rs", "shell"),
            said("adapters/cli/hook.rs", "shell"),
            said("adapters/mcp.rs", "shell"),
        ]
    );
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

/// The most lines a source file may have.
const MOST_LINES: usize = 300;

/// Files longer than [`MOST_LINES`] from before the limit, each with the
/// length it may not pass while it is split. Lower an entry as its file
/// shrinks, and remove it once the file is short enough.
const LONG: &[(&str, usize)] = &[
    ("src/adapters/cli.rs", 502),
    ("src/adapters/cli/hook.rs", 323),
    ("src/adapters/cli/progress.rs", 692),
    ("src/adapters/cli/render.rs", 1040),
    ("src/adapters/mcp.rs", 1308),
    ("src/core/powershell.rs", 515),
    ("src/core/powershell/read.rs", 308),
    ("src/core/powershell/run.rs", 629),
    ("src/core/secrets.rs", 452),
    ("src/core/shell.rs", 1036),
    ("src/core/shell/lex.rs", 723),
    ("src/core/shell/read.rs", 355),
    ("src/core/shell/run.rs", 629),
    ("src/core/shell/tables.rs", 323),
    ("src/core/shell/writers.rs", 631),
    ("src/engine/exec.rs", 910),
    ("src/engine/exec/escape/macos.rs", 490),
    ("src/engine/progress.rs", 346),
    ("src/engine/sandbox.rs", 388),
    ("src/service/audit.rs", 442),
    ("src/service/bundle.rs", 1393),
    ("src/service/doctor.rs", 302),
    ("src/service/guard/git.rs", 623),
    ("src/service/guard/judge.rs", 701),
    ("src/service/guard/rules.rs", 751),
    ("src/service/keys.rs", 448),
    ("src/service/mod.rs", 1039),
    ("src/service/search.rs", 331),
    ("tests/architecture.rs", 516),
    ("tests/guard_corpus.rs", 364),
];

/// The Rust files under `dir`, relative to the crate root with `/` between
/// their parts, and their lengths in lines.
fn lengths(dir: &str, found: &mut Vec<(String, usize)>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut pending = vec![root.join(dir)];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                let relative = path.strip_prefix(root).unwrap();
                let name = relative
                    .iter()
                    .map(|part| part.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                let text = std::fs::read_to_string(&path).unwrap();
                found.push((name, text.lines().count()));
            }
        }
    }
}

/// Every file longer than it may be. An entry of [`LONG`] may outlast the
/// length it names: a proof held to the tests of an earlier revision reads
/// that revision's list, and must still pass on the work that split a file.
fn length_violations(found: &[(String, usize)], long: &[(&str, usize)]) -> Vec<String> {
    let mut violations = Vec::new();
    for (name, lines) in found {
        match long.iter().find(|(path, _)| path == name) {
            None if *lines > MOST_LINES => violations.push(format!(
                "{name} has {lines} lines, more than {MOST_LINES}; split it"
            )),
            Some((_, most)) if lines > most => violations.push(format!(
                "{name} has {lines} lines, more than the {most} it had; split it instead"
            )),
            _ => {}
        }
    }
    violations
}

#[test]
fn no_source_file_grows_past_300_lines() {
    let mut found = Vec::new();
    lengths("src", &mut found);
    lengths("tests", &mut found);
    let violations = length_violations(&found, LONG);
    assert!(violations.is_empty(), "{violations:#?}");
}

#[test]
fn the_length_rule_itself_names_each_way_to_break_it() {
    let found = [
        ("src/short.rs".to_string(), 300),
        ("src/new.rs".to_string(), 301),
        ("src/old.rs".to_string(), 401),
        ("src/smaller.rs".to_string(), 350),
        ("src/same.rs".to_string(), 500),
    ];
    let long = [
        ("src/old.rs", 400),
        ("src/smaller.rs", 360),
        ("src/same.rs", 500),
        ("src/gone.rs", 900),
    ];
    assert_eq!(
        length_violations(&found, &long),
        [
            "src/new.rs has 301 lines, more than 300; split it",
            "src/old.rs has 401 lines, more than the 400 it had; split it instead",
        ]
    );
}
