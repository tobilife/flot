//! The README ships in English, Korean and Chinese, and every release updates
//! all three, and the Claude Code plugin with them. A version bump that leaves
//! one behind fails here, before a tag is pushed.

const READMES: [&str; 3] = [
    "README.md",
    "docs/readme/README_ko.md",
    "docs/readme/README_zh.md",
];

fn read(path: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|error| panic!("cannot read {path}: {error}"))
}

#[test]
fn every_readme_names_the_release_being_built() {
    let version = env!("CARGO_PKG_VERSION");
    for readme in READMES {
        assert!(
            read(readme).contains(&format!("**{version}**")),
            "{readme} does not name release {version}; update all three READMEs"
        );
    }
}

#[test]
fn every_readme_links_the_other_two() {
    let links = [
        (
            "README.md",
            ["docs/readme/README_ko.md", "docs/readme/README_zh.md"],
        ),
        (
            "docs/readme/README_ko.md",
            ["../../README.md", "README_zh.md"],
        ),
        (
            "docs/readme/README_zh.md",
            ["../../README.md", "README_ko.md"],
        ),
    ];
    for (readme, others) in links {
        let text = read(readme);
        for other in others {
            assert!(
                text.contains(&format!("]({other})")),
                "{readme} does not link {other}"
            );
        }
    }
}

#[test]
fn the_claude_code_plugin_names_the_release_being_built() {
    let manifest: serde_json::Value =
        serde_json::from_str(&read("plugins/claude-code/.claude-plugin/plugin.json"))
            .expect("plugin.json is JSON");
    assert_eq!(
        manifest["version"],
        env!("CARGO_PKG_VERSION"),
        "plugins/claude-code/.claude-plugin/plugin.json names another version"
    );
    let marketplace: serde_json::Value =
        serde_json::from_str(&read(".claude-plugin/marketplace.json"))
            .expect("marketplace.json is JSON");
    let entry = &marketplace["plugins"][0];
    assert_eq!(entry["name"], manifest["name"]);
    assert_eq!(entry["source"], "./plugins/claude-code");
}

#[test]
fn the_github_action_installs_the_release_being_built() {
    let action = read("action.yml");
    let wanted = format!(
        "  version:\n    description: flot release to install, 0.2.4 or later, which carry a build provenance attestation; `path` uses the flot already on PATH.\n    default: \"{v}\"\n",
        v = env!("CARGO_PKG_VERSION")
    );
    assert!(
        action.contains(&wanted),
        "action.yml installs another version by default"
    );
    let uses = format!("uses: tobilife/flot@v{}", env!("CARGO_PKG_VERSION"));
    assert!(
        read("docs/ci.md").contains(&uses),
        "docs/ci.md uses another release of the action"
    );
}

/// CI and the release build with the toolchain rust-toolchain.toml pins, so
/// a run signed here is reused there; the MSRV check builds with
/// `rust-version`.
#[test]
fn the_workflows_build_with_the_pinned_toolchain() {
    let msrv = read("Cargo.toml")
        .lines()
        .find_map(|line| line.strip_prefix("rust-version = "))
        .expect("Cargo.toml names rust-version")
        .trim_matches('"')
        .to_string();
    assert!(
        read("rust-toolchain.toml").contains("\nchannel = \""),
        "rust-toolchain.toml pins a channel"
    );
    for workflow in [".github/workflows/ci.yml", ".github/workflows/release.yml"] {
        let lines: Vec<String> = read(workflow).lines().map(str::to_string).collect();
        for (at, line) in lines.iter().enumerate() {
            let Some(version) = line.trim().strip_prefix("- uses: dtolnay/rust-toolchain@") else {
                continue;
            };
            if version == msrv {
                assert!(
                    lines
                        .iter()
                        .any(|l| l.trim() == format!("RUSTUP_TOOLCHAIN: \"{msrv}\"")),
                    "{workflow}:{}: the MSRV job sets RUSTUP_TOOLCHAIN over rust-toolchain.toml",
                    at + 1
                );
                continue;
            }
            assert_eq!(version, "master", "{workflow}:{}", at + 1);
            assert!(
                lines[at + 1..]
                    .iter()
                    .take(3)
                    .any(|l| l.trim() == "toolchain: ${{ steps.rust.outputs.toolchain }}"),
                "{workflow}:{}: installs the toolchain rust-toolchain.toml pins",
                at + 1
            );
        }
    }
}

/// flot.toml proves each claim with its own `cargo test` selection, so a test
/// target that no step selects would never be run by the self-check.
#[test]
fn every_test_target_is_run_by_a_step_of_the_self_check() {
    let flow: toml::Value = toml::from_str(&read("flot.toml")).expect("flot.toml is TOML");
    let proofs: Vec<Vec<&str>> = flow["step"]
        .as_array()
        .expect("flot.toml has steps")
        .iter()
        .filter_map(|step| step.get("verify")?.as_str())
        .map(|verify| verify.split_whitespace().collect())
        .collect();
    assert!(
        proofs.iter().flatten().any(|word| *word == "--lib"),
        "no step of flot.toml proves the unit tests (`--lib`)"
    );
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    for entry in std::fs::read_dir(dir).expect("tests/ is readable") {
        let path = entry.expect("tests/ lists").path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            let target = path.file_stem().unwrap().to_string_lossy().into_owned();
            assert!(
                proofs.iter().any(|words| words
                    .windows(2)
                    .any(|pair| pair == ["--test", target.as_str()])),
                "no step of flot.toml proves tests/{target}.rs (`--test {target}`)"
            );
        }
    }
}
