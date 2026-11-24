//! End-to-end behavior through the real binary: the contract a user sees.

use std::path::Path;
use std::process::Command;

struct Project {
    dir: tempfile::TempDir,
}

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Output {
    fn ok(&self) -> &Self {
        assert!(
            self.code == 0,
            "expected exit 0, got {}\nstdout:\n{}\nstderr:\n{}",
            self.code,
            self.stdout,
            self.stderr
        );
        self
    }

    fn has(&self, needle: &str) -> &Self {
        assert!(
            self.stdout.contains(needle) || self.stderr.contains(needle),
            "expected to find {needle:?}\nstdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    fn lacks(&self, needle: &str) -> &Self {
        assert!(
            !(self.stdout.contains(needle) || self.stderr.contains(needle)),
            "did not expect {needle:?}\nstdout:\n{}",
            self.stdout
        );
        self
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout).expect("stdout is JSON")
    }
}

impl Project {
    fn new(flow: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("flot.toml"), flow).expect("write flow");
        Project { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, contents).expect("write file");
    }

    /// Where this project's signing key lives. Every invocation is pinned to
    /// it, so no test can read or create a key in the real home directory.
    fn key_path(&self) -> std::path::PathBuf {
        self.path().join(".test-keys").join("signing.key")
    }

    /// This project's store, opened the way its flot opens it.
    fn store(&self) -> flot::store::Store {
        let text = std::fs::read_to_string(self.path().join(".test-keys/host.secret"))
            .expect("flot created the host secret");
        let secret: [u8; 32] = hex::decode(text.trim()).unwrap().try_into().unwrap();
        flot::store::Store::open(self.path(), flot::store::HostSecret::from_bytes(secret)).unwrap()
    }

    fn flot(&self, args: &[&str]) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_flot"))
            .args(args)
            .current_dir(self.path())
            .env("FLOT_SIGNING_KEY", self.key_path())
            .output()
            .expect("flot runs");
        Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

const PROVEN: &str = r#"
[flow]
name = "proven"

[[step]]
id = "write"
run = "echo flot > artifact.txt"
inputs = ["seed.txt"]
claims = ["artifact.txt contains flot"]
verify = "grep -q flot artifact.txt"
"#;

#[test]
fn a_proven_claim_passes_with_exit_zero() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project
        .flot(&["run"])
        .ok()
        .has("PASSED")
        .has("claims 1v/0u/0r");
}

#[test]
fn a_claim_without_a_verify_command_can_never_pass() {
    let project = Project::new(
        r#"
[flow]
name = "unproven"

[[step]]
id = "assert"
run = "true"
claims = ["the migration is safe"]
"#,
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 2, "stdout:\n{}", output.stdout);
    output
        .has("UNVERIFIED")
        .has("claims are unverified")
        .lacks("PASSED");
}

#[test]
fn a_refuted_claim_fails_the_run() {
    let project = Project::new(
        r#"
[flow]
name = "refuted"

[[step]]
id = "lie"
run = "echo nothing > out.txt"
claims = ["out.txt mentions flot"]
verify = "grep -q flot out.txt"
"#,
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.has("FAILED").has("claims 0v/0u/1r");
}

#[test]
fn a_failed_step_cancels_its_dependents_before_they_run() {
    let project = Project::new(
        r#"
[flow]
name = "cancel"

[[step]]
id = "broken"
run = "exit 7"

[[step]]
id = "downstream"
run = "touch should-not-exist.txt"
needs = ["broken"]
"#,
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1);
    output
        .has("cancelled")
        .has("dependency broken (failed) did not hold");
    assert!(
        !project.path().join("should-not-exist.txt").exists(),
        "a cancelled step must not cause side effects"
    );
}

#[test]
fn unchanged_inputs_reuse_the_receipt_and_reproduce_the_root() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    let first = project.flot(&["run", "--json"]);
    first.ok();
    let second = project.flot(&["run", "--json"]);
    second.ok();

    let first_root = first.json()["manifest"]["root_digest"].clone();
    let second_root = second.json()["manifest"]["root_digest"].clone();
    assert_eq!(
        first_root, second_root,
        "a reused run must fold to the same root"
    );
    assert_eq!(second.json()["steps"][0]["cached"], serde_json::json!(true));
    assert_eq!(first.json()["steps"][0]["cached"], serde_json::json!(false));
}

#[test]
fn changing_a_declared_input_invalidates_the_cache() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    project.write("seed.txt", "two");
    let second = project.flot(&["run", "--json"]);
    second.ok();
    assert_eq!(
        second.json()["steps"][0]["cached"],
        serde_json::json!(false)
    );
}

#[test]
fn a_failed_step_is_retried_instead_of_replayed_from_the_cache() {
    let project = Project::new(
        r#"
[flow]
name = "flaky"

[[step]]
id = "flaky"
run = "if [ -f attempted ]; then echo recovered; else touch attempted; exit 1; fi"
"#,
    );
    assert_eq!(project.flot(&["run"]).code, 1);
    let second = project.flot(&["run", "--json"]);
    second.ok();
    assert_eq!(
        second.json()["steps"][0]["cached"],
        serde_json::json!(false)
    );
    assert_eq!(second.json()["steps"][0]["status"], "ok");
}

#[test]
fn a_refuted_claim_is_re_proved_on_the_next_run() {
    let project = Project::new(
        r#"
[flow]
name = "refuted-then-fixed"

[[step]]
id = "check"
run = "true"
claims = ["the fix is in place"]
verify = "test -f fixed"
"#,
    );
    assert_eq!(project.flot(&["run"]).code, 1);
    project.write("fixed", "");
    project.flot(&["run"]).ok().has("PASSED");
}

#[test]
fn force_re_runs_a_step_that_would_have_been_reused() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    let forced = project.flot(&["run", "--force", "--json"]);
    forced.ok();
    assert_eq!(
        forced.json()["steps"][0]["cached"],
        serde_json::json!(false)
    );
}

#[test]
fn diff_reports_identical_runs_and_real_changes() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    let runs = project.flot(&["runs", "--json"]);
    runs.ok();
    let first_id = runs.json()["runs"][0]["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    project.flot(&["run"]).ok();
    project
        .flot(&["diff", &first_id, "latest"])
        .ok()
        .has("identical");

    project.write("seed.txt", "changed");
    project.flot(&["run"]).ok();
    let changed = project.flot(&["diff", &first_id, "latest"]);
    assert_eq!(changed.code, 2);
    changed.has("receipt changed").has("write");
}

#[test]
fn verify_authenticates_a_recorded_run() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    project
        .flot(&["verify"])
        .ok()
        .has("integrity: intact")
        .has("verdict passed");
}

#[test]
fn verify_detects_an_edited_receipt() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();

    let receipts = project.path().join(".flot/receipts");
    let entry = std::fs::read_dir(&receipts)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = std::fs::read_to_string(&entry).unwrap();
    let forged = text
        .replace("\"unverified\"", "\"verified\"")
        .replace("\"exit_code\": 0", "\"exit_code\": 0, \"forged\": true");
    std::fs::write(&entry, forged.replace("echo flot", "echo tampered")).unwrap();

    let output = project.flot(&["verify"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.has("integrity: BROKEN").has("does not match its id");
}

#[test]
fn complete_output_stays_retrievable_when_the_inline_view_is_trimmed() {
    let project = Project::new(
        r#"
[flow]
name = "budget"
budget = { max_inline_bytes = 16 }

[[step]]
id = "chatty"
run = "yes flot | head -c 5000"
claims = ["the step produced output"]
verify = "true"
"#,
    );
    let run = project.flot(&["run", "--json"]);
    run.ok();
    let step = &run.json()["steps"][0];
    assert_eq!(step["stdout_truncated"], serde_json::json!(true));
    assert_eq!(step["stdout_inline"].as_str().unwrap().len(), 16);

    let receipt_id = step["receipt_id"].as_str().unwrap().to_string();
    let receipt = project.flot(&["receipt", &receipt_id]);
    receipt.ok();
    let object = receipt.json()["identity"]["stdout_digest"]
        .as_str()
        .unwrap()
        .to_string();
    let complete = project.flot(&["cat", &object]);
    complete.ok();
    assert_eq!(
        complete.stdout.len(),
        5000,
        "the complete stream is recoverable"
    );
}

#[test]
fn a_step_over_budget_is_a_timeout_not_a_success() {
    let project = Project::new(
        r#"
[flow]
name = "slow"

[[step]]
id = "hang"
run = "sleep 30"
budget = { timeout_secs = 1 }
claims = ["the work finished"]
verify = "true"
"#,
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output
        .has("timeout")
        .has("claims are unverified because the step did not succeed");
}

#[test]
fn plan_names_steps_that_claim_without_proof_and_runs_nothing() {
    let project = Project::new(
        r#"
[flow]
name = "planned"

[[step]]
id = "risky"
run = "touch plan-side-effect.txt"
claims = ["nothing broke"]
"#,
    );
    project.flot(&["plan"]).ok().has("NO PROOF").has("risky");
    assert!(!project.path().join("plan-side-effect.txt").exists());
}

#[test]
fn only_runs_the_requested_step_and_its_dependencies() {
    let project = Project::new(
        r#"
[flow]
name = "subset"

[[step]]
id = "first"
run = "echo first"

[[step]]
id = "second"
run = "echo second"
needs = ["first"]

[[step]]
id = "unrelated"
run = "touch unrelated-ran.txt"
"#,
    );
    project.flot(&["run", "--only", "second"]).ok();
    assert!(!project.path().join("unrelated-ran.txt").exists());
    let show = project.flot(&["show", "--json"]);
    show.ok();
    let steps = show.json()["steps"].as_array().unwrap().len();
    assert_eq!(steps, 2);
    show.has("partial run");
}

const FAN_OUT: &str = r#"
[flow]
name = "fan-out"

[[step]]
id = "seed"
run = "echo seed"

[[step]]
id = "left"
run = "sleep 0.6; echo left"
needs = ["seed"]

[[step]]
id = "right"
run = "sleep 0.6; echo right"
needs = ["seed"]

[[step]]
id = "join"
run = "echo join"
needs = ["left", "right"]
claims = ["both branches finished before the join"]
verify = "true"
"#;

fn run_duration_ms(project: &Project, args: &[&str]) -> u64 {
    let output = project.flot(args);
    output.ok();
    output.json()["manifest"]["duration_ms"].as_u64().unwrap()
}

#[test]
fn independent_steps_overlap_only_when_concurrency_is_allowed() {
    let sequential = Project::new(FAN_OUT);
    let serial_ms = run_duration_ms(&sequential, &["run", "--json"]);
    assert!(
        serial_ms >= 1200,
        "one job must not overlap the two 600ms branches, took {serial_ms}ms"
    );

    let parallel = Project::new(FAN_OUT);
    let parallel_ms = run_duration_ms(&parallel, &["run", "--jobs", "2", "--json"]);
    assert!(
        parallel_ms < 1100,
        "two jobs must overlap the branches, took {parallel_ms}ms"
    );
}

#[test]
fn concurrency_does_not_change_receipts_or_the_run_root() {
    let sequential = Project::new(FAN_OUT);
    let serial = sequential.flot(&["run", "--json"]);
    serial.ok();

    let parallel = Project::new(FAN_OUT);
    let concurrent = parallel.flot(&["run", "--jobs", "4", "--json"]);
    concurrent.ok();

    assert_eq!(
        serial.json()["manifest"]["root_digest"],
        concurrent.json()["manifest"]["root_digest"],
        "the same work must fold to the same root at any concurrency"
    );
    let serial_steps: Vec<String> = serial.json()["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["step_id"].as_str().unwrap().to_string())
        .collect();
    let concurrent_steps: Vec<String> = concurrent.json()["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["step_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        serial_steps, concurrent_steps,
        "steps are reported in dependency order, not completion order"
    );
}

#[test]
fn a_parallel_run_states_what_concurrency_does_not_prove() {
    let project = Project::new(FAN_OUT);
    project
        .flot(&["run", "--jobs", "2"])
        .ok()
        .has("ran concurrently");
}

#[test]
fn max_parallel_in_the_flow_file_enables_overlap_without_a_flag() {
    let project = Project::new(
        &FAN_OUT.replace("name = \"fan-out\"", "name = \"fan-out\"\nmax_parallel = 2"),
    );
    let elapsed = run_duration_ms(&project, &["run", "--json"]);
    assert!(elapsed < 1100, "took {elapsed}ms");
}

#[test]
fn a_failing_branch_cancels_its_own_dependents_while_the_other_branch_completes() {
    let project = Project::new(
        r#"
[flow]
name = "half-broken"
max_parallel = 4

[[step]]
id = "broken"
run = "exit 1"

[[step]]
id = "after-broken"
run = "touch must-not-exist.txt"
needs = ["broken"]

[[step]]
id = "healthy"
run = "touch healthy-ran.txt"
"#,
    );
    let output = project.flot(&["run", "--json"]);
    assert_eq!(output.code, 1);
    let statuses: Vec<(String, String)> = output.json()["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            (
                step["step_id"].as_str().unwrap().to_string(),
                step["status"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(statuses.contains(&("broken".into(), "failed".into())));
    assert!(statuses.contains(&("after-broken".into(), "cancelled".into())));
    assert!(statuses.contains(&("healthy".into(), "ok".into())));
    assert!(project.path().join("healthy-ran.txt").exists());
    assert!(!project.path().join("must-not-exist.txt").exists());
}

#[test]
fn init_writes_a_runnable_starter_flow() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project { dir };
    std::fs::remove_file(project.path().join("flot.toml")).ok();
    project.flot(&["init"]).ok().has("wrote");
    project.flot(&["run"]).has("inspect");
    project.flot(&["init"]).has("already exists");
}

#[test]
fn the_mcp_server_handshakes_and_runs_the_flow_over_stdio() {
    use std::io::Write;

    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");

    let mut child = Command::new(env!("CARGO_BIN_EXE_flot"))
        .arg("mcp")
        .current_dir(project.path())
        .env("FLOT_SIGNING_KEY", project.key_path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("flot mcp starts");

    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"flot_run","arguments":{}}}"#,
    ];
    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        for request in requests {
            writeln!(stdin, "{request}").unwrap();
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("mcp server exits");
    assert!(output.status.success());

    let replies: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is JSON-RPC"))
        .collect();
    assert_eq!(replies.len(), 3, "the notification must not be answered");
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "flot");
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 8);
    assert_eq!(
        replies[2]["result"]["structuredContent"]["manifest"]["verdict"],
        "passed"
    );
    assert!(project.path().join(".flot/runs").exists());
}

#[test]
fn a_bundle_moves_a_run_to_another_host_and_still_authenticates() {
    let source = Project::new(PROVEN);
    source.write("seed.txt", "one");
    source.flot(&["run"]).ok();
    let bundle_path = source.path().join("run.flotbundle.json");
    source
        .flot(&["bundle", "-o", bundle_path.to_str().unwrap()])
        .ok()
        .has("receipts");

    let run_id = run_id(&source);

    let target = Project::new(PROVEN);
    target
        .flot(&["import", bundle_path.to_str().unwrap()])
        .ok()
        .has("imported run");
    target
        .flot(&["verify", &run_id])
        .ok()
        .has("integrity: intact")
        .has("verdict passed");
    target.flot(&["runs"]).ok().has("(imported)");
    let runs = target.flot(&["runs", "--json"]);
    assert_eq!(runs.json()["runs"][0]["imported"], true);

    // `latest` means work done here; someone else's run is never the default.
    let bare = target.flot(&["verify"]);
    assert_eq!(bare.code, 3);
    bare.has("no run was executed here yet");
    target.flot(&["run"]).ok();
    let mine = target.flot(&["show", "--json"]).json();
    assert_ne!(mine["run_id"], run_id.as_str());
    assert_eq!(mine["imported"], false);

    // Importing the same bundle again changes nothing and does not move `latest`.
    target.flot(&["import", bundle_path.to_str().unwrap()]).ok();
    assert_eq!(
        target.flot(&["show", "--json"]).json()["run_id"],
        mine["run_id"]
    );

    // A host signs only what it ran.
    target.flot(&["key", "init"]).ok();
    let resign = target.flot(&["bundle", "--sign", &run_id]);
    assert_eq!(resign.code, 3);
    resign.has("can be bundled again but not signed");
}

#[test]
fn a_forged_bundle_is_refused_and_leaves_the_store_empty() {
    let source = Project::new(PROVEN);
    source.write("seed.txt", "one");
    source.flot(&["run"]).ok();
    let bundle_path = source.path().join("run.flotbundle.json");
    source
        .flot(&["bundle", "-o", bundle_path.to_str().unwrap()])
        .ok();

    let text = std::fs::read_to_string(&bundle_path).unwrap();
    let forged = text.replacen("grep -q flot artifact.txt", "true", 1);
    assert_ne!(text, forged, "the forgery must change the bundle");
    std::fs::write(&bundle_path, forged).unwrap();

    let target = Project::new(PROVEN);
    let output = target.flot(&["import", bundle_path.to_str().unwrap()]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.has("does not match its id");
    target.flot(&["runs"]).ok().has("no runs recorded");
}

fn billed(id: &str, dollars: &str) -> String {
    format!(
        "[[step]]\nid = \"{id}\"\nrun = \"echo '{{\\\"input_tokens\\\": 100, \\\"output_tokens\\\": 10, \\\"cost_usd\\\": {dollars}}}' > \\\"$FLOT_METRICS\\\"\"\n\n"
    )
}

fn billed_flow(name: &str, steps: &[(&str, &str)]) -> String {
    let mut flow = format!("[flow]\nname = \"{name}\"\n\n");
    for (id, dollars) in steps {
        flow.push_str(&billed(id, dollars));
    }
    flow
}

#[test]
fn steps_report_spend_through_flot_metrics() {
    let project = Project::new(&billed_flow("billed", &[("a", "0.25"), ("b", "0.5")]));
    let run = project.flot(&["run", "--json"]);
    run.ok();
    let spend = &run.json()["manifest"]["spend"];
    assert_eq!(spend["cost_micro_usd"], 750_000);
    assert_eq!(spend["input_tokens"], 200);
    assert_eq!(spend["output_tokens"], 20);
    assert_eq!(spend["reporting_steps"], 2);
    assert_eq!(spend["silent_steps"], 0);
    assert_eq!(run.json()["steps"][0]["metrics"]["cost_micro_usd"], 250_000);
    project
        .flot(&["run", "--force"])
        .ok()
        .has("spent $0.750000");
}

#[test]
fn the_cost_ceiling_stops_new_steps_once_reported_spend_reaches_it() {
    let project = Project::new(&billed_flow(
        "ceiling",
        &[("a", "0.6"), ("b", "0.6"), ("c", "0.6")],
    ));
    let output = project.flot(&["run", "--max-cost", "1.0", "--json"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    let statuses: Vec<String> = output.json()["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["status"].as_str().unwrap().to_string())
        .collect();
    // a spends 0.6; b starts below the ceiling and spends 0.6; c is refused.
    assert_eq!(statuses, vec!["ok", "ok", "cancelled"]);
    let limitations = output.json()["manifest"]["limitations"].to_string();
    assert!(
        limitations.contains("cost ceiling of $1.000000 reached"),
        "{limitations}"
    );
    assert!(
        limitations.contains("spent $1.200000 against a $1.000000 ceiling"),
        "{limitations}"
    );
}

#[test]
fn reused_receipts_cost_nothing_and_pass_under_any_ceiling() {
    let project = Project::new(&billed_flow("reuse", &[("a", "0.6"), ("b", "0.6")]));
    project.flot(&["run"]).ok();
    let reused = project.flot(&["run", "--max-cost", "0.01", "--json"]);
    reused.ok();
    let spend = &reused.json()["manifest"]["spend"];
    assert_eq!(spend["cost_micro_usd"], 0);
    assert_eq!(spend["reused_cost_micro_usd"], 1_200_000);
}

#[test]
fn a_ceiling_names_steps_whose_spend_it_cannot_see() {
    let project = Project::new(
        r#"
[flow]
name = "silent"
max_cost_usd = 5

[[step]]
id = "quiet"
run = "echo no metrics here"
"#,
    );
    project
        .flot(&["run"])
        .ok()
        .has("1 executed step(s) reported no metrics");
}

#[test]
fn a_malformed_metrics_file_is_reported_and_treated_as_unknown_spend() {
    let project = Project::new(
        r#"
[flow]
name = "garbled"

[[step]]
id = "garbled"
run = "echo '{\"cost_usd\": \"lots\"}' > \"$FLOT_METRICS\""
"#,
    );
    let run = project.flot(&["run", "--json"]);
    run.ok();
    assert_eq!(run.json()["manifest"]["spend"]["silent_steps"], 1);
    assert!(run.json()["manifest"]["limitations"]
        .to_string()
        .contains("metrics file ignored"));
}

#[test]
fn a_usage_error_exits_with_the_invalid_request_code_not_unverified() {
    let project = Project::new(PROVEN);
    let output = project.flot(&["run", "--no-such-flag"]);
    assert_eq!(output.code, 3, "exit 2 is reserved for unverified claims");
    assert_eq!(project.flot(&["--help"]).code, 0);
    assert_eq!(project.flot(&["--version"]).code, 0);
}

#[test]
fn an_invalid_cost_ceiling_is_refused() {
    let project = Project::new(PROVEN);
    assert_eq!(project.flot(&["run", "--max-cost", "-1"]).code, 3);
    let bad_flow =
        Project::new(&PROVEN.replace("name = \"proven\"", "name = \"proven\"\nmax_cost_usd = -2"));
    let output = bad_flow.flot(&["run"]);
    assert_eq!(output.code, 3);
    output.has("max_cost_usd");
}

#[test]
fn the_truncation_hint_names_a_digest_that_cat_can_read() {
    let project = Project::new(
        r#"
[flow]
name = "hint"
budget = { max_inline_bytes = 8 }

[[step]]
id = "chatty"
run = "yes flot | head -c 300"
"#,
    );
    let run = project.flot(&["run"]);
    run.ok();
    let hint = run
        .stdout
        .lines()
        .find(|line| line.contains("flot cat"))
        .expect("a trimmed step prints a recovery hint");
    let digest = hint
        .split_whitespace()
        .find(|word| word.starts_with("sha256:"))
        .unwrap();
    assert_eq!(project.flot(&["cat", digest]).ok().stdout.len(), 300);
}

fn public_key(project: &Project) -> String {
    let out = project.flot(&["key", "init", "--json"]);
    out.ok();
    out.json()["public_key"].as_str().unwrap().to_string()
}

fn signed_bundle(source: &Project) -> std::path::PathBuf {
    source.write("seed.txt", "one");
    source.flot(&["run"]).ok();
    let path = source.path().join("signed.flotbundle.json");
    source
        .flot(&["bundle", "--sign", "-o", path.to_str().unwrap()])
        .ok()
        .has("signed by ed25519:");
    path
}

#[test]
fn key_init_is_idempotent_and_never_prints_the_secret() {
    let project = Project::new(PROVEN);
    let first = project.flot(&["key", "init"]);
    first.ok().has("created signing key");
    let secret = std::fs::read_to_string(project.key_path()).unwrap();
    first.lacks(secret.trim());
    let again = project.flot(&["key", "init"]);
    again.ok().has("using signing key").lacks(secret.trim());
    assert_eq!(std::fs::read_to_string(project.key_path()).unwrap(), secret);
    project.flot(&["key", "show"]).ok().lacks(secret.trim());
}

#[test]
fn signing_without_a_key_explains_how_to_make_one() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    let output = project.flot(&["bundle", "--sign"]);
    assert_eq!(output.code, 3);
    output.has("flot key init");
}

/// The id of a project's newest run.
fn run_id(project: &Project) -> String {
    project.flot(&["runs", "--json"]).json()["runs"][0]["run_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_trusted_signer_is_named_on_import_and_on_every_later_verify() {
    let source = Project::new(PROVEN);
    let key = public_key(&source);
    let bundle = signed_bundle(&source);

    let target = Project::new(PROVEN);
    target.flot(&["trust", "add", "alice", &key]).ok();
    target
        .flot(&["import", "--require-trusted", bundle.to_str().unwrap()])
        .ok()
        .has("authorship: trusted")
        .has("trusted as alice");
    target
        .flot(&["verify", &run_id(&source)])
        .ok()
        .has("integrity: intact")
        .has("authorship: trusted");

    // Revoking trust changes the judgement, not the evidence.
    target.flot(&["trust", "remove", "alice"]).ok();
    target
        .flot(&["verify", &run_id(&source)])
        .ok()
        .has("authorship: untrusted")
        .has("not on the trust list");
}

#[test]
fn require_trusted_refuses_an_unknown_signer_and_leaves_nothing_behind() {
    let source = Project::new(PROVEN);
    public_key(&source);
    let bundle = signed_bundle(&source);

    let target = Project::new(PROVEN);
    let output = target.flot(&["import", "--require-trusted", bundle.to_str().unwrap()]);
    assert_eq!(output.code, 3);
    output.has("bundle is untrusted");
    target.flot(&["runs"]).ok().has("no runs recorded");
}

#[test]
fn editing_a_signed_bundle_is_detected_even_when_every_digest_still_matches() {
    let source = Project::new(PROVEN);
    let key = public_key(&source);
    let bundle = signed_bundle(&source);

    // Change only the run's recorded duration: receipts, objects and the root
    // all stay valid, so integrity alone would accept this.
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bundle).unwrap()).unwrap();
    let duration = value["run"]["duration_ms"].as_u64().unwrap();
    value["run"]["duration_ms"] = serde_json::json!(duration + 1);
    std::fs::write(&bundle, serde_json::to_string(&value).unwrap()).unwrap();

    let target = Project::new(PROVEN);
    target.flot(&["trust", "add", "alice", &key]).ok();
    let output = target.flot(&["import", bundle.to_str().unwrap()]);
    assert_eq!(output.code, 1);
    output.has("altered after signing");
}

#[test]
fn a_weak_or_malformed_key_cannot_be_trusted() {
    let project = Project::new(PROVEN);
    let identity = format!("ed25519:01{}", "00".repeat(31));
    let output = project.flot(&["trust", "add", "mallory", &identity]);
    assert_eq!(output.code, 3);
    output.has("weak");
    assert_eq!(
        project.flot(&["trust", "add", "x", "ed25519:nothex"]).code,
        3
    );
    project
        .flot(&["trust", "list"])
        .ok()
        .has("no trusted signers");
}

const GUARDED: &str = r#"
[flow]
name = "guarded"

[[step]]
id = "build"
run = "echo built > artifact.txt"
claims = ["the acceptance check passes"]
verify = "sh check.sh"
verify_inputs = ["check.sh"]
"#;

#[test]
fn tightening_the_proof_re_proves_instead_of_reusing_an_old_verdict() {
    let project = Project::new(GUARDED);
    project.write("check.sh", "grep -q built artifact.txt\n");
    project.flot(&["run"]).ok().has("PASSED");

    // The work is unchanged, but the proof now demands something it lacks.
    project.write("check.sh", "grep -q shipped artifact.txt\n");
    let output = project.flot(&["run", "--json"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    assert_eq!(
        output.json()["steps"][0]["cached"],
        serde_json::json!(false)
    );
    assert_eq!(output.json()["manifest"]["claims"]["refuted"], 1);
}

#[test]
fn a_step_that_rewrites_its_own_proof_cannot_verify() {
    let project = Project::new(&GUARDED.replace(
        "run = \"echo built > artifact.txt\"",
        "run = \"echo built > artifact.txt; echo 'exit 0' > check.sh\"",
    ));
    project.write("check.sh", "exit 1\n");
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 2, "stdout:\n{}", output.stdout);
    output
        .has("UNVERIFIED")
        .has("the step changed its own proof (check.sh)");
}

#[test]
fn a_step_that_edits_its_declared_input_is_reported() {
    let project = Project::new(
        r#"
[flow]
name = "self-editing"

[[step]]
id = "edit"
run = "echo changed > spec.md"
inputs = ["spec.md"]
"#,
    );
    project.write("spec.md", "original\n");
    project
        .flot(&["run"])
        .ok()
        .has("the step changed its own declared inputs (spec.md)");
}

#[test]
fn work_built_on_a_refuted_claim_is_not_run() {
    let project = Project::new(
        r#"
[flow]
name = "refuted-upstream"

[[step]]
id = "implement"
run = "echo draft > artifact.txt"
claims = ["artifact.txt is final"]
verify = "grep -q final artifact.txt"

[[step]]
id = "review"
run = "touch review-ran.txt"
needs = ["implement"]
"#,
    );
    let output = project.flot(&["run", "--json"]);
    assert_eq!(output.code, 1);
    assert_eq!(output.json()["steps"][1]["status"], "cancelled");
    assert!(!project.path().join("review-ran.txt").exists());
    assert!(output.json()["manifest"]["limitations"]
        .to_string()
        .contains("dependency implement (claims refuted) did not hold"));
}

#[test]
fn an_unproven_dependency_does_not_block_its_dependents() {
    let project = Project::new(
        r#"
[flow]
name = "unproven-upstream"

[[step]]
id = "implement"
run = "true"
claims = ["no proof was declared for this"]

[[step]]
id = "review"
run = "touch review-ran.txt"
needs = ["implement"]
"#,
    );
    assert_eq!(project.flot(&["run"]).code, 2);
    assert!(project.path().join("review-ran.txt").exists());
}

#[test]
fn verify_inputs_without_a_verify_command_is_refused() {
    let project = Project::new(&GUARDED.replace("verify = \"sh check.sh\"\n", ""));
    let output = project.flot(&["plan"]);
    assert_eq!(output.code, 3);
    output.has("verify_inputs without a verify command");
}

#[test]
fn an_invalid_flow_is_refused_with_the_request_exit_code() {
    let project = Project::new(
        r#"
[flow]
name = "cyclic"

[[step]]
id = "a"
run = "true"
needs = ["b"]

[[step]]
id = "b"
run = "true"
needs = ["a"]
"#,
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 3);
    output.has("dependency cycle");
}

#[test]
fn the_flow_file_is_found_from_a_subdirectory() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    std::fs::create_dir_all(project.path().join("nested/deep")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_flot"))
        .arg("run")
        .current_dir(project.path().join("nested/deep"))
        .env("FLOT_SIGNING_KEY", project.key_path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(
        project.path().join(".flot/runs").exists(),
        "the store belongs to the flow root, not the current directory"
    );
}

const PRODUCES: &str = r#"
[flow]
name = "produces"

[[step]]
id = "make"
run = "mkdir -p build && tr a-z A-Z < in.txt > build/out.txt"
inputs = ["in.txt"]
outputs = ["build/*.txt"]
claims = ["the output is the input in upper case"]
verify = "grep -qx HELLO build/out.txt"
"#;

fn read(project: &Project, relative: &str) -> String {
    std::fs::read_to_string(project.path().join(relative)).expect("file exists")
}

#[test]
fn a_reused_receipt_puts_back_the_files_it_proved() {
    let project = Project::new(PRODUCES);
    project.write("in.txt", "hello\n");
    project.flot(&["run"]).ok().has("> build/out.txt");

    // Collection keeps output objects: they are evidence of a recorded run.
    project.flot(&["gc"]).ok();
    std::fs::remove_dir_all(project.path().join("build")).unwrap();
    project
        .flot(&["run"])
        .ok()
        .has("cached")
        .has("output build/out.txt was missing and was restored from its receipt");
    assert_eq!(read(&project, "build/out.txt"), "HELLO\n");

    // Nothing to put back: no note.
    project.flot(&["run"]).ok().lacks("restored");
}

#[test]
fn a_changed_output_is_neither_overwritten_nor_vouched_for() {
    let project = Project::new(
        r#"
[flow]
name = "drift"

[[step]]
id = "make"
run = "test -f out.txt || echo generated > out.txt"
outputs = ["out.txt"]
claims = ["out.txt says generated"]
verify = "grep -qx generated out.txt"
"#,
    );
    project.flot(&["run"]).ok();
    project.write("out.txt", "hand edit\n");
    // The old receipt proved other content, so it is not reused, and the edit
    // is not overwritten: the step runs again and its proof judges the file.
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output
        .has("output out.txt changed since its receipt; the step runs again")
        .has("ran      make")
        .has("claims 0v/0u/1r");
    assert_eq!(read(&project, "out.txt"), "hand edit\n");
}

#[cfg(unix)]
#[test]
fn outputs_are_never_read_or_written_through_a_symbolic_link() {
    let outside = tempfile::tempdir().unwrap();
    let project = Project::new(PRODUCES);
    project.write("in.txt", "hello\n");
    project.flot(&["run"]).ok();

    // Swap the output directory for a link to somewhere else: reuse must not
    // write through it.
    std::fs::remove_dir_all(project.path().join("build")).unwrap();
    std::os::unix::fs::symlink(outside.path(), project.path().join("build")).unwrap();
    // The cached receipt is passed over rather than restored through the
    // link; the step runs, and what it wrote through the link is not recorded.
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output
        .has("output build/out.txt is reached through a symbolic link; the cached receipt is not reused")
        .has("ran      make")
        .lacks("was missing and was restored");

    // A link planted where an output is expected is never recorded.
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "do not export\n").unwrap();
    let leak = Project::new(
        "[flow]\nname = \"leak\"\n\n[[step]]\nid = \"a\"\nrun = \"true\"\noutputs = [\"*.txt\"]\n",
    );
    std::os::unix::fs::symlink(&secret, leak.path().join("leak.txt")).unwrap();
    let output = leak.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output
        .has("output leak.txt is reached through a symbolic link and was not recorded")
        .lacks("> leak.txt");
}

#[test]
fn a_success_that_skips_a_declared_output_fails() {
    let project = Project::new(
        r#"
[flow]
name = "forgetful"

[[step]]
id = "make"
run = "echo done"
outputs = ["report.txt"]
claims = ["the report exists"]
verify = "true"
"#,
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output
        .has("FAILED")
        .has("produced no file for declared output report.txt")
        .has("claims 0v/1u/0r");
}

#[test]
fn outputs_travel_with_a_bundle() {
    let source = Project::new(PRODUCES);
    source.write("in.txt", "hello\n");
    source.flot(&["run"]).ok();
    let bundle = source.path().join("run.flotbundle.json");
    source
        .flot(&["bundle", "-o", bundle.to_str().unwrap()])
        .ok();

    let target = Project::new(PRODUCES);
    target.write("in.txt", "hello\n");
    // Unsigned evidence can be inspected, but it never stands in for work.
    target
        .flot(&["import", bundle.to_str().unwrap()])
        .ok()
        .has("not reusable");
    target.flot(&["run"]).ok().has("ran      make");
    assert_eq!(read(&target, "build/out.txt"), "HELLO\n");
}

#[test]
fn a_trusted_bundle_restores_the_files_it_proved() {
    let source = Project::new(PRODUCES);
    source.write("in.txt", "hello\n");
    source.flot(&["run"]).ok();
    let key = source
        .flot(&["key", "init"])
        .ok()
        .stdout
        .lines()
        .last()
        .unwrap()
        .to_string();
    let bundle = source.path().join("run.flotbundle.json");
    source
        .flot(&["bundle", "--sign", "-o", bundle.to_str().unwrap()])
        .ok();

    let target = Project::new(PRODUCES);
    target.write("in.txt", "hello\n");
    target.flot(&["trust", "add", "alice", &key]).ok();
    target
        .flot(&["import", bundle.to_str().unwrap()])
        .ok()
        .has("reusable: local runs may reuse");
    target
        .flot(&["run"])
        .ok()
        .has("cached")
        .has("was missing and was restored");
    assert_eq!(read(&target, "build/out.txt"), "HELLO\n");
}

#[test]
fn a_bundle_cannot_name_a_run_outside_the_store() {
    let source = Project::new(PROVEN);
    source.write("seed.txt", "one");
    source.flot(&["run"]).ok();
    let path = source.path().join("run.flotbundle.json");
    source.flot(&["bundle", "-o", path.to_str().unwrap()]).ok();
    let mut bundle: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    bundle["run"]["run_id"] = "../../../escaped".into();
    std::fs::write(&path, bundle.to_string()).unwrap();

    let target = Project::new(PROVEN);
    let output = target.flot(&["import", path.to_str().unwrap()]);
    assert_eq!(output.code, 1);
    output.has("is not derived from its start time and root");
    assert!(!target.path().join("..").join("escaped.json").exists());
}

#[test]
fn verify_recomputes_the_verdict_instead_of_reading_it() {
    let project = Project::new(
        "[flow]\nname = \"v\"\n\n[[step]]\nid = \"s\"\nrun = \"true\"\nclaims = [\"x\"]\nverify = \"false\"\n",
    );
    assert_eq!(project.flot(&["run"]).code, 1);
    let latest = std::fs::read_to_string(project.path().join(".flot/latest")).unwrap();
    let run_file = project.path().join(format!(".flot/runs/{latest}.json"));
    let mut run: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&run_file).unwrap()).unwrap();
    run["verdict"] = "passed".into();
    std::fs::write(&run_file, run.to_string()).unwrap();

    let output = project.flot(&["verify"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.has("manifest verdict is passed but its receipts make it failed");
}

#[test]
fn a_failing_line_in_a_multi_line_step_fails_the_step() {
    let project = Project::new(
        "[flow]\nname = \"m\"\n\n[[step]]\nid = \"agent\"\nrun = \"\"\"\nfalse\necho recorded anyway\n\"\"\"\n",
    );
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.has("failed").lacks("recorded anyway");
}

#[test]
fn a_step_that_backgrounds_work_does_not_hold_the_run_open() {
    let project = Project::new(
        "[flow]\nname = \"bg\"\nbudget = { timeout_secs = 60 }\n\n[[step]]\nid = \"leak\"\nrun = \"echo started; sleep 30 &\"\n",
    );
    let started = std::time::Instant::now();
    project
        .flot(&["run"])
        .ok()
        .has("left processes running when it exited; they were stopped");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn a_receipt_naming_an_undeclared_file_is_never_reused() {
    use flot::core::receipt::{FileDigest, Receipt};

    let project = Project::new(PRODUCES);
    project.write("in.txt", "hello\n");
    project.flot(&["run"]).ok();

    // Forge what a hostile bundle could import: an authentic receipt for the
    // same step key that also lists a file outside the step's outputs.
    let store = project.store();
    let run = store.resolve_run("latest").unwrap();
    let honest = store.load_receipt(&run.steps[0].receipt_id).unwrap();
    let mut identity = honest.identity.clone();
    identity.outputs.push(FileDigest {
        executable: false,
        path: "planted.sh".into(),
        digest: store.put_bytes(b"echo pwned\n").unwrap(),
    });
    let forged = Receipt::seal(identity, honest.observed.clone());
    store.put_receipt(&forged).unwrap();
    store
        .remember(&forged.identity.step_key, &forged.receipt_id, &[])
        .unwrap();

    std::fs::remove_dir_all(project.path().join("build")).unwrap();
    project.flot(&["run"]).ok().has("ran      make");
    assert!(!project.path().join("planted.sh").exists());
}

#[test]
fn unordered_steps_producing_one_file_are_reported() {
    let project = Project::new(
        r#"
[flow]
name = "clash"

[[step]]
id = "a"
run = "echo a > shared.txt"
outputs = ["shared.txt"]

[[step]]
id = "b"
run = "echo b > shared.txt"
outputs = ["shared.txt"]

[[step]]
id = "c"
run = "echo c > shared.txt"
needs = ["a", "b"]
outputs = ["shared.txt"]
"#,
    );
    project
        .flot(&["run"])
        .ok()
        .has("steps a and b both produce shared.txt and neither needs the other")
        .lacks("steps a and c")
        .lacks("steps b and c");
}

#[test]
fn outputs_must_stay_inside_the_flow_and_off_the_proof() {
    for (outputs, verify_inputs, message) in [
        (
            "[\"../escape.txt\"]",
            "[]",
            "must be a relative path inside the flow root",
        ),
        (
            "[\"/tmp/abs.txt\"]",
            "[]",
            "must be a relative path inside the flow root",
        ),
        (
            "[\"check.sh\"]",
            "[\"check.sh\"]",
            "a step cannot produce its own proof",
        ),
    ] {
        let project = Project::new(&format!(
            "[flow]\nname = \"bad\"\n\n[[step]]\nid = \"s\"\nrun = \"true\"\nclaims = [\"x\"]\nverify = \"sh check.sh\"\noutputs = {outputs}\nverify_inputs = {verify_inputs}\n"
        ));
        let output = project.flot(&["plan"]);
        assert_eq!(output.code, 3, "{outputs}: {}", output.stderr);
        output.has(message);
    }
}

#[test]
fn step_output_cannot_drive_the_terminal() {
    let project = Project::new(
        "[flow]\nname = \"esc\"\n\n[[step]]\nid = \"s\"\nrun = \"printf 'a\\\\033[2Jb\\\\n'\"\n",
    );
    let output = project.flot(&["run"]);
    output.ok().has("a\\u{1b}[2Jb");
    assert!(!output.stdout.contains('\u{1b}'));
}

#[test]
fn a_step_that_cannot_run_is_recorded_and_the_run_still_closes() {
    let project = Project::new(
        r#"
[flow]
name = "unrunnable"

[[step]]
id = "first"
run = "echo hi"

[[step]]
id = "after"
needs = ["first"]
run = "echo after"
"#,
    );
    // Make it impossible to give an executed step its metrics path.
    std::fs::create_dir_all(project.path().join(".flot")).unwrap();
    std::fs::write(project.path().join(".flot/tmp"), "not a directory").unwrap();
    let output = project.flot(&["run"]);
    assert_eq!(
        output.code, 1,
        "stdout:\n{}\nstderr:\n{}",
        output.stdout, output.stderr
    );
    output
        .has("ran      first                failed")
        .has("first: could not run:")
        .has("skipped  after")
        .has("FAILED");
    let latest = std::fs::read_to_string(project.path().join(".flot/latest")).unwrap();
    assert!(
        output.stdout.contains(latest.trim()),
        "the run was recorded"
    );
}

#[test]
fn re_executing_the_same_work_leaves_earlier_runs_verifiable() {
    let project = Project::new(
        "[flow]\nname = \"again\"\n\n[[step]]\nid = \"ok\"\nrun = \"echo hi\"\n\n[[step]]\nid = \"bad\"\nrun = \"exit 1\"\nallow_failure = false\n",
    );
    // Same identity each time, different timing: the receipt file is shared,
    // so each run must keep its own observation.
    assert_eq!(project.flot(&["run"]).code, 1);
    let first = std::fs::read_to_string(project.path().join(".flot/latest")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(project.flot(&["run", "--force"]).code, 1);
    let output = project.flot(&["verify", first.trim()]);
    output.has("integrity: intact");
    let bundle = project.path().join("first.json");
    project
        .flot(&["bundle", first.trim(), "-o", bundle.to_str().unwrap()])
        .ok();
    let target =
        Project::new("[flow]\nname = \"again\"\n\n[[step]]\nid = \"ok\"\nrun = \"echo hi\"\n");
    target.flot(&["import", bundle.to_str().unwrap()]).ok();
}

#[test]
fn an_out_of_range_timestamp_is_refused_not_a_crash() {
    let source = Project::new(PROVEN);
    source.write("seed.txt", "one");
    source.flot(&["run"]).ok();
    let path = source.path().join("run.flotbundle.json");
    source.flot(&["bundle", "-o", path.to_str().unwrap()]).ok();
    let text = std::fs::read_to_string(&path).unwrap();
    let mut bundle: serde_json::Value = serde_json::from_str(&text).unwrap();
    let started = bundle["receipts"][0]["observed"]["started_at_ms"].to_string();
    let forged = text.replacen(
        &format!("\"started_at_ms\": {started}"),
        "\"started_at_ms\": 18446744073709551616",
        1,
    );
    assert_ne!(forged, text, "the field was rewritten");
    bundle = serde_json::Value::Null;
    let _ = bundle;
    std::fs::write(&path, forged).unwrap();
    let target = Project::new(PROVEN);
    let output = target.flot(&["import", path.to_str().unwrap()]);
    assert_eq!(output.code, 3, "stderr:\n{}", output.stderr);
    output.lacks("panicked");
}

fn step_flow(body: &str) -> String {
    format!("[flow]\nname = \"t\"\n\n[[step]]\nid = \"s\"\n{body}\n")
}

#[test]
fn adding_a_claim_to_a_cached_step_runs_its_proof() {
    let project = Project::new(&step_flow("run = \"true\"\nverify = \"false\""));
    project.flot(&["run"]).ok();
    let flow = std::fs::read_to_string(project.path().join("flot.toml")).unwrap();
    project.write("flot.toml", &format!("{flow}claims = [\"it works\"]\n"));
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.has("ran      s").has("claims 0v/0u/1r");
}

#[test]
fn allow_failure_keeps_the_run_going_but_proves_nothing() {
    let project = Project::new(&step_flow(
        "run = \"exit 7\"\nallow_failure = true\nclaims = [\"done\"]\nverify = \"true\"",
    ));
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 2, "stdout:\n{}", output.stdout);
    output
        .has("exited 7 but the step is declared allow_failure")
        .has("claims 0v/1u/0r");
}

#[test]
fn a_proof_that_changes_what_it_judges_verifies_nothing() {
    let project = Project::new(&step_flow(
        "run = \"echo draft > out.txt\"\noutputs = [\"out.txt\"]\nclaims = [\"final\"]\nverify = \"echo final > out.txt; grep -qx final out.txt\"",
    ));
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 2, "stdout:\n{}", output.stdout);
    output.has("files the proof judged changed while it ran (out.txt)");
}

#[test]
fn a_proof_killed_by_a_signal_is_unverified_not_refuted() {
    let project = Project::new(&step_flow(
        "run = \"true\"\nclaims = [\"x\"]\nverify = \"kill -9 $$\"",
    ));
    let output = project.flot(&["run"]);
    assert_eq!(output.code, 2, "stdout:\n{}", output.stdout);
    output.has("killed by a signal").has("claims 0v/1u/0r");
}

#[cfg(unix)]
#[test]
fn a_restored_output_keeps_its_executable_bit() {
    let project = Project::new(&step_flow(
        "run = \"printf '#!/bin/sh\\\\necho hi\\\\n' > tool.sh; chmod +x tool.sh\"\noutputs = [\"tool.sh\"]\nclaims = [\"runs\"]\nverify = \"./tool.sh\"",
    ));
    project.flot(&["run"]).ok().has("claims 1v/0u/0r");
    std::fs::remove_file(project.path().join("tool.sh")).unwrap();
    project.flot(&["run"]).ok().has("cached   s");
    assert!(std::process::Command::new(project.path().join("tool.sh"))
        .status()
        .unwrap()
        .success());
}

#[test]
fn why_a_receipt_was_passed_over_is_not_sealed_into_the_next_one() {
    let project = Project::new(
        "[flow]\nname = \"n\"\n\n[[step]]\nid = \"make\"\nrun = \"echo generated > out.txt\"\noutputs = [\"out.txt\"]\n\n[[step]]\nid = \"use\"\nneeds = [\"make\"]\nrun = \"cat out.txt\"\n",
    );
    let first = project.flot(&["run", "--json"]).json();
    project.write("out.txt", "edit\n");
    let second = project.flot(&["run", "--json"]);
    second.has("output out.txt changed since its receipt");
    let second = second.json();
    // Identical work, identical receipt: the dependent is reused.
    assert_eq!(
        first["steps"][0]["receipt_id"],
        second["steps"][0]["receipt_id"]
    );
    assert_eq!(second["steps"][1]["cached"], true);
}

#[test]
fn a_failed_attempt_does_not_evict_an_earlier_success() {
    let project = Project::new(&step_flow("run = \"echo work\""));
    project.flot(&["run"]).ok();
    assert_eq!(project.flot(&["run", "--force", "--max-cost", "0"]).code, 1);
    project.flot(&["run"]).ok().has("cached   s");
}

#[test]
fn plan_sees_the_files_reuse_will_restore() {
    let project = Project::new(
        "[flow]\nname = \"p\"\n\n[[step]]\nid = \"gen\"\nrun = \"echo data > gen.txt\"\noutputs = [\"gen.txt\"]\n\n[[step]]\nid = \"use\"\nneeds = [\"gen\"]\nrun = \"cat gen.txt\"\ninputs = [\"gen.txt\"]\n",
    );
    project.flot(&["run"]).ok();
    std::fs::remove_file(project.path().join("gen.txt")).unwrap();
    let plan = project.flot(&["plan", "--json"]).json();
    assert_eq!(plan["steps"][1]["cached"], true, "{plan}");
    project.flot(&["run"]).ok().has("cached   use");

    // A receipt that cannot be checked is a miss in plan, as in run.
    std::fs::remove_file(project.path().join("gen.txt")).unwrap();
    std::fs::create_dir(project.path().join("gen.txt")).unwrap();
    project
        .flot(&["plan"])
        .ok()
        .has("cached receipt not reused");
}

#[test]
fn the_store_is_never_a_step_input_and_the_root_is_not_a_pattern() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("k[1]");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("flot.toml"),
        step_flow(
            "run = \"echo x > out.txt\"\ninputs = [\"**/*\", \".flot/**/*\"]\noutputs = [\"out.txt\"]",
        ),
    )
    .unwrap();
    let flot = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_flot"))
            .args(args)
            .current_dir(&root)
            .env("FLOT_SIGNING_KEY", parent.path().join("key"))
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    flot(&["run"]);
    flot(&["run"]);
    let third = flot(&["run"]);
    assert!(third.contains("cached   s"), "{third}");
    assert!(
        third.contains("`.flot/**/*` matched no readable file"),
        "{third}"
    );
}

#[test]
fn hostile_metrics_never_crash_or_hang_a_run() {
    let project = Project::new(
        "[flow]\nname = \"m\"\n\n[[step]]\nid = \"a\"\nrun = \"echo '{\\\"input_tokens\\\": 18446744073709551615}' > \\\"$FLOT_METRICS\\\"\"\n\n[[step]]\nid = \"b\"\nrun = \"echo '{\\\"input_tokens\\\": 18446744073709551615}' > \\\"$FLOT_METRICS\\\"\"\n\n[[step]]\nid = \"c\"\nrun = \"mkfifo \\\"$FLOT_METRICS\\\"\"\n",
    );
    project
        .flot(&["run"])
        .ok()
        .has("c: metrics file ignored: it is not a regular file");
}

#[cfg(unix)]
#[test]
fn stopping_flot_stops_the_step_and_still_records_the_run() {
    let project = Project::new(
        "[flow]\nname = \"sig\"\n\n[[step]]\nid = \"s\"\nrun = \"sleep 30; touch survived\"\n\n[[step]]\nid = \"t\"\nneeds = [\"s\"]\nrun = \"echo after\"\n",
    );
    let child = Command::new(env!("CARGO_BIN_EXE_flot"))
        .arg("run")
        .current_dir(project.path())
        .env("FLOT_SIGNING_KEY", project.key_path())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let started = std::time::Instant::now();
    let output = child.wait_with_output().unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("stopped because flot was asked to stop"),
        "{stdout}"
    );
    assert!(
        stdout.contains("t: not run: flot was asked to stop"),
        "{stdout}"
    );
    project.flot(&["verify"]).has("integrity: intact");
}

const TWO_STEPS: &str = r#"
[flow]
name = "two"

[[step]]
id = "a"
run = "echo first"

[[step]]
id = "b"
run = "echo second"
"#;

fn receipt_file(project: &Project, step: usize) -> std::path::PathBuf {
    let run = project.flot(&["show", "--json"]).json();
    let id = run["steps"][step]["receipt_id"]
        .as_str()
        .unwrap()
        .to_string();
    project
        .path()
        .join(".flot/receipts")
        .join(format!("{}.json", id.trim_start_matches("sha256:")))
}

#[test]
fn a_receipt_file_holding_another_receipt_fails_verify() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let (a, b) = (receipt_file(&project, 0), receipt_file(&project, 1));
    assert!(a.exists() && b.exists(), "{a:?}");
    std::fs::copy(&b, &a).unwrap();
    let output = project.flot(&["verify"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.lacks("integrity: intact").has("holds receipt");
}

#[test]
fn verify_reads_every_evidence_object_not_just_its_name() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let run = project.flot(&["show", "--json"]).json();
    let receipt = project.flot(&["receipt", run["steps"][0]["receipt_id"].as_str().unwrap()]);
    let stdout = receipt.json()["identity"]["stdout_digest"]
        .as_str()
        .unwrap()
        .trim_start_matches("sha256:")
        .to_string();
    std::fs::write(
        project
            .path()
            .join(".flot/objects")
            .join(&stdout[..2])
            .join(&stdout),
        "tampered\n",
    )
    .unwrap();
    let output = project.flot(&["verify"]);
    assert_eq!(output.code, 1, "stdout:\n{}", output.stdout);
    output.lacks("integrity: intact").has("is not intact");
}

#[test]
fn a_copied_store_brings_evidence_but_not_decisions() {
    let source = Project::new(PROVEN);
    source.write("seed.txt", "one");
    source.flot(&["key", "init"]).ok();
    let key = source.flot(&["key", "show", "--json"]).json()["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    source.flot(&["trust", "add", "mallory", &key]).ok();
    source.flot(&["run"]).ok().has("ran      write");

    // Another host (its own secret) receives the whole store, cache and
    // trust list included, the way a committed `.flot` would arrive.
    let target = Project::new(PROVEN);
    target.write("seed.txt", "one");
    let copied = std::process::Command::new("cp")
        .args(["-r"])
        .arg(source.path().join(".flot"))
        .arg(target.path())
        .status()
        .unwrap();
    assert!(copied.success());

    // The trust list is refused, not believed, by everything that would
    // judge authorship with it.
    for command in [&["trust", "list"][..], &["verify", &run_id(&source)][..]] {
        let refused = target.flot(command);
        assert_eq!(refused.code, 1, "{command:?}");
        refused.has("not written by this host");
    }
    std::fs::remove_file(target.path().join(".flot/trust.json")).unwrap();
    // The evidence still audits.
    target
        .flot(&["verify", &run_id(&source)])
        .ok()
        .has("integrity: intact");
    // And the cache does not stand in for running the step here.
    target.flot(&["run"]).ok().has("ran      write");
    target.flot(&["run"]).ok().has("cached   write");
}

#[test]
fn a_run_is_named_by_its_id_or_a_prefix_never_by_a_fragment() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let id = run_id(&project);
    let (stamp, root) = id.split_once('-').unwrap();
    project.flot(&["show", &stamp[..9]]).ok();
    project.flot(&["show", &root[..6]]).ok();
    let fragment = project.flot(&["show", &root[3..9]]);
    assert_eq!(fragment.code, 3);
    fragment.has("no run id or root starts with");
    let short = project.flot(&["show", &root[..2]]);
    assert_eq!(short.code, 3);
    short.has("too short");
}

#[test]
fn a_run_file_holding_another_run_is_refused() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let first = run_id(&project);
    project.write("flot.toml", &TWO_STEPS.replace("echo second", "echo third"));
    project.flot(&["run"]).ok();
    let second = run_id(&project);
    let runs = project.path().join(".flot/runs");
    std::fs::copy(
        runs.join(format!("{second}.json")),
        runs.join(format!("{first}.json")),
    )
    .unwrap();
    let output = project.flot(&["show", &first]);
    assert_eq!(output.code, 1);
    output.has(&format!("holds run {second}"));
}

#[test]
fn reading_a_project_leaves_no_store_behind() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["runs"]).ok().has("no runs recorded");
    project.flot(&["plan"]).ok();
    project.flot(&["gc"]).ok();
    assert!(!project.path().join(".flot").exists());
}

#[test]
fn gc_sweeps_what_only_describes_a_removed_run() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["key", "init"]).ok();
    project.flot(&["run"]).ok();
    project.flot(&["bundle", "--sign"]).ok();
    let id = run_id(&project);
    let attestation = project.path().join(format!(".flot/attestations/{id}.json"));
    assert!(attestation.exists());
    std::fs::remove_file(project.path().join(format!(".flot/runs/{id}.json"))).unwrap();
    std::fs::create_dir_all(project.path().join(".flot/tmp")).unwrap();
    std::fs::write(project.path().join(".flot/tmp/left-over"), "x").unwrap();
    project.flot(&["gc"]).ok();
    assert!(!attestation.exists());
    assert!(!project.path().join(".flot/tmp/left-over").exists());
}

#[test]
fn json_output_is_valid_json_whatever_a_step_prints() {
    // DEL, a C1 control (CSI) and a bidi isolate: none are escaped by JSON
    // itself, and each must reach a JSON reader intact.
    let project = Project::new(&step_flow(
        "run = \"printf 'x\\\\177y\\\\302\\\\233z\\\\342\\\\201\\\\250w'\"",
    ));
    let output = project.flot(&["run", "--json"]);
    output.ok();
    let report = output.json();
    assert_eq!(
        report["steps"][0]["stdout_inline"], "x\u{7f}y\u{9b}z\u{2068}w",
        "{}",
        output.stdout
    );
    assert!(
        !output.stdout.contains('\u{9b}'),
        "the terminal never sees CSI"
    );
}

#[test]
fn show_json_keeps_claim_totals_and_claims_per_step_apart() {
    let project = Project::new(PROVEN);
    project.write("seed.txt", "one");
    project.flot(&["run"]).ok();
    let output = project.flot(&["show", "--json"]);
    assert_eq!(output.stdout.matches("\n  \"claims\"").count(), 1);
    let detail = output.json();
    assert_eq!(detail["claims"]["verified"], 1);
    assert_eq!(detail["step_claims"]["write"][0]["status"], "verified");
}

#[test]
fn a_named_flow_file_is_found_the_same_way_however_it_is_written() {
    let project = Project::new(TWO_STEPS);
    std::fs::create_dir_all(project.path().join("sub")).unwrap();
    std::fs::rename(
        project.path().join("flot.toml"),
        project.path().join("sub/flow.toml"),
    )
    .unwrap();
    project.flot(&["-f", "sub/flow.toml", "run"]).ok();
    let absolute = project.path().join("sub/flow.toml");
    project
        .flot(&["-f", absolute.to_str().unwrap(), "run"])
        .ok()
        .has("cached   a");
    assert!(project.path().join("sub/.flot").exists());
    assert!(!project.path().join(".flot").exists());

    let missing = project.flot(&["-f", "nope.toml", "runs"]);
    assert_eq!(missing.code, 3);
    missing.has("does not exist");
}

#[test]
fn init_in_a_subdirectory_leaves_the_parent_project_alone() {
    let project = Project::new(TWO_STEPS);
    std::fs::create_dir_all(project.path().join("sub")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_flot"))
        .args(["init", "--force"])
        .current_dir(project.path().join("sub"))
        .env("FLOT_SIGNING_KEY", project.key_path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(read(&project, "flot.toml"), TWO_STEPS);
    assert!(project.path().join("sub/flot.toml").exists());
}

#[test]
fn an_environment_fault_exits_4_not_as_a_bad_request() {
    use std::os::unix::fs::PermissionsExt;
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let runs = project.path().join(".flot/runs");
    std::fs::set_permissions(&runs, std::fs::Permissions::from_mode(0o555)).unwrap();
    let output = project.flot(&["run", "--force"]);
    std::fs::set_permissions(&runs, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(output.code, 4, "stderr:\n{}", output.stderr);
}

#[test]
fn an_unreadable_run_is_named_and_does_not_hide_the_others() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let broken = run_id(&project);
    project.write("flot.toml", &TWO_STEPS.replace("echo second", "echo third"));
    project.flot(&["run"]).ok();
    let good = run_id(&project);
    project.write(&format!(".flot/runs/{broken}.json"), "{");

    let runs = project.flot(&["runs"]);
    assert_eq!(runs.code, 1, "stdout:\n{}", runs.stdout);
    runs.has(&good).has(&format!("unreadable  {broken}"));

    // gc cannot know what the broken run references, so it collects nothing.
    let gc = project.flot(&["gc"]);
    assert_eq!(gc.code, 1);
    gc.has("gc stopped").has(&format!("run {broken}"));

    // Nor what a broken receipt references.
    std::fs::remove_file(project.path().join(format!(".flot/runs/{broken}.json"))).unwrap();
    std::fs::write(receipt_file(&project, 0), "{").unwrap();
    let gc = project.flot(&["gc"]);
    assert_eq!(gc.code, 1);
    gc.has("gc stopped").has("receipt sha256:");
}

#[test]
fn a_missing_file_named_in_a_request_is_a_bad_request() {
    let project = Project::new(TWO_STEPS);
    let output = project.flot(&["import", "absent.flotbundle.json"]);
    assert_eq!(output.code, 3, "stderr:\n{}", output.stderr);
}

#[test]
fn a_tampered_receipt_is_refused_by_name_not_printed() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["run"]).ok();
    let path = receipt_file(&project, 0);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replacen("echo first", "echo forged", 1)).unwrap();
    let id = project.flot(&["show", "--json"]).json()["steps"][0]["receipt_id"]
        .as_str()
        .unwrap()
        .to_string();
    let output = project.flot(&["receipt", &id]);
    assert_eq!(output.code, 1);
    assert!(output.stdout.is_empty());
    output.has("does not match its body");
    // `show` does not repeat what the altered receipt says either.
    let detail = project.flot(&["show", "--json"]).json();
    assert!(detail["step_claims"].get("a").is_none(), "{detail}");
    assert!(detail["step_claims"].get("b").is_some(), "{detail}");
}

#[test]
fn zero_jobs_is_refused() {
    let project = Project::new(TWO_STEPS);
    assert_eq!(project.flot(&["run", "-j", "0"]).code, 3);
}

#[test]
fn plan_says_when_a_step_declares_no_inputs() {
    let project = Project::new(TWO_STEPS);
    project.flot(&["plan"]).ok().has("declares no inputs");
}
