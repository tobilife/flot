//! A run called through MCP is held to the revision the flow names, and
//! `flot_verify` says when only the change to a proof keeps it from passing.

use serde_json::{json, Value};

use super::{call, project};

const ANCHORED: &str = r#"
[flow]
name = "anchored"
proofs_from = "main"

[[step]]
id = "work"
run = "echo 1 > out.txt"
claims = ["out.txt holds 1"]
verify = "sh check.sh"
verify_inputs = ["check.sh"]
"#;

/// Run git in `dir`, as a host with no git identity of its own.
fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", dir.join("no-global-gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn limitations(run: &Value) -> Vec<String> {
    run["structuredContent"]["manifest"]["limitations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|limitation| limitation.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_run_through_mcp_is_held_to_the_flows_revision_and_says_when_only_proofs_differ() {
    let (dir, server) = project(ANCHORED);
    let root = dir.path();
    std::fs::write(root.join("check.sh"), "grep -q 1 out.txt\n").unwrap();
    std::fs::write(root.join(".gitignore"), ".flot\nout.txt\n").unwrap();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "base"]);

    // The flow's revision holds, though no argument named it.
    let run = call(&server, "flot_run", json!({}));
    assert_eq!(run["structuredContent"]["result"], "passed", "{run}");
    assert!(
        limitations(&run)
            .iter()
            .any(|limitation| limitation.starts_with("proofs checked against main")),
        "{run}"
    );

    // The proof is weakened in the work: the run is unverified, and verify
    // says the change to the proof is all that keeps it from passing. That
    // is for a person to review, not an error of the tool.
    std::fs::write(root.join("check.sh"), "true\n").unwrap();
    let run = call(&server, "flot_run", json!({}));
    assert_eq!(run["structuredContent"]["result"], "unverified", "{run}");
    assert_eq!(run["isError"], false, "{run}");
    let audit = call(&server, "flot_verify", json!({}));
    let report = &audit["structuredContent"];
    assert_eq!(report["only_proofs_differ"], true, "{audit}");
    assert_eq!(report["result"], "unverified", "{audit}");
    assert_eq!(report["anchor"]["drifted"][0]["step_id"], "work", "{audit}");
    assert_eq!(audit["isError"], false, "{audit}");

    // A failing step is more than a changed proof.
    std::fs::write(
        root.join("flot.toml"),
        ANCHORED.replace("echo 1 > out.txt", "exit 1"),
    )
    .unwrap();
    call(&server, "flot_run", json!({}));
    let audit = call(&server, "flot_verify", json!({}));
    assert_eq!(
        audit["structuredContent"]["only_proofs_differ"], false,
        "{audit}"
    );
    assert_eq!(audit["structuredContent"]["result"], "failed", "{audit}");
}
