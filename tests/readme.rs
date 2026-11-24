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
        "  version:\n    description: flot release to install, such as {v}; `path` uses the flot already on PATH.\n    default: \"{v}\"\n",
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
