mod record;

use self::record::{scenario_record, SCENARIO_RECORD};
use crate::core::flow::Flow;
use crate::engine::progress::{self, Event};
use crate::engine::runner::{RunOptions, Runner};
use crate::store::{HostSecret, Store};

/// A flow that settles a step each way a run can: `build` passes with an
/// output and a priced metrics file, `lint` fails, `ship` is not run
/// because `lint` did not hold, `tolerant` fails as `allow_failure`
/// lets it and collects limitations, `flaky` holds at its second attempt
/// and `pick` keeps the cheaper of two candidates.
const SCENARIO: &str = r#"
[flow]
name = "scenario"
max_cost_usd = 1.0

[[step]]
id = "build"
run = '''printf 'built\n' > out.txt; printf '{"cost_usd": 0.25, "input_tokens": 7, "retries": 1}' > "$FLOT_METRICS"; echo building'''
inputs = ["src.txt"]
outputs = ["out.txt"]
claims = ["out.txt says built"]
verify = "sh check.sh"
verify_inputs = ["check.sh"]

[[step]]
id = "lint"
run = "echo 'src.txt: 2 problems' >&2; exit 3"
inputs = ["src.txt"]
claims = ["src.txt is clean"]
verify = "sh lint_check.sh"
verify_inputs = ["lint_check.sh"]

[[step]]
id = "ship"
needs = ["build", "lint"]
run = "cat out.txt"
inputs = ["out.txt"]
claims = ["shipped"]
verify = "true"

[[step]]
id = "tolerant"
run = '''echo trying; printf 'trace\n' > trace.log; printf 'stray\n' > stray.txt; exit 1'''
inputs = ["notes/*.md"]
outputs = ["report.txt"]
may_change = ["trace.log"]
allow_failure = true

[[step]]
id = "flaky"
run = 'test "$FLOT_ATTEMPT" = 2'
inputs = ["src.txt"]
attempts = 2
claims = ["it held"]
verify = "true"

[[step]]
id = "pick"
run = '''printf '%s\n' "$FLOT_CANDIDATE" > pick.txt; if [ "$FLOT_CANDIDATE" = fast ]; then cost=0.01; else cost=0.02; fi; printf '{"cost_usd": %s}' "$cost" > "$FLOT_METRICS"'''
inputs = ["src.txt"]
outputs = ["pick.txt"]
candidates = ["slow", "fast"]
claims = ["pick.txt names a candidate"]
verify = "test -s pick.txt"
"#;

/// Every byte two runs of `SCENARIO` leave behind: each run's manifest
/// and step reports, every receipt in the store, every cache entry.
/// Limitation order is part of every receipt id, so this pins it too.
#[test]
fn a_scenario_leaves_exactly_these_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |path: &str, text: &str| std::fs::write(root.join(path), text).unwrap();
    write("src.txt", "fn main() {}\n");
    write("check.sh", "grep -q built out.txt\n");
    write("lint_check.sh", "exit 0\n");
    let flow = Flow::parse(SCENARIO).unwrap();
    let store = Store::open(root, HostSecret::from_bytes([7; 32])).unwrap();
    let run = || {
        Runner::new(&flow, root, &store)
            .run(&RunOptions::default())
            .unwrap()
    };
    let first = run();
    // The second run reuses what held and puts back the output deleted
    // here; `lint` runs again and its proof has changed.
    std::fs::remove_file(root.join("out.txt")).unwrap();
    write("lint_check.sh", "exit 1\n");
    let second = run();

    let record = scenario_record(&flow, &store, &[first, second]);
    for (line, (actual, expected)) in record.lines().zip(SCENARIO_RECORD.lines()).enumerate() {
        assert_eq!(actual, expected, "line {}", line + 1);
    }
    assert_eq!(record, SCENARIO_RECORD);
}

/// A run that reports its progress leaves the same bytes, and tells
/// each attempt and candidate it executes as started, then finished.
#[test]
fn a_run_told_to_a_sink_leaves_the_same_bytes() {
    use std::sync::{Arc, Mutex};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |path: &str, text: &str| std::fs::write(root.join(path), text).unwrap();
    write("src.txt", "fn main() {}\n");
    write("check.sh", "grep -q built out.txt\n");
    write("lint_check.sh", "exit 0\n");
    let flow = Flow::parse(SCENARIO).unwrap();
    let store = Store::open(root, HostSecret::from_bytes([7; 32])).unwrap();
    let told = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&told);
    let options = RunOptions {
        progress: progress::Sink::new(move |event| kept.lock().unwrap().push(event.clone())),
        ..RunOptions::default()
    };
    let run = || Runner::new(&flow, root, &store).run(&options).unwrap();
    let first = run();
    std::fs::remove_file(root.join("out.txt")).unwrap();
    write("lint_check.sh", "exit 1\n");
    let second = run();
    assert_eq!(
        scenario_record(&flow, &store, &[first, second]),
        SCENARIO_RECORD
    );

    // One step at a time, so each start is followed by its finish.
    let told = told.lock().unwrap();
    let mut executions = Vec::new();
    let mut open = None;
    for event in told.iter() {
        match event {
            Event::StepStarted {
                step,
                attempt,
                candidate,
                ..
            } => {
                assert_eq!(open, None, "{event:?}");
                open = Some((step.clone(), *attempt, candidate.clone()));
            }
            Event::StepFinished {
                step,
                attempt: 1..,
                candidate,
                ..
            } => {
                let started = open.take().expect("a finish follows a start");
                assert_eq!((&started.0, &started.2), (step, candidate));
                executions.push(started);
            }
            _ => {}
        }
    }
    let pick = |name: &str| ("pick".to_string(), 1, Some(name.to_string()));
    let plain = |step: &str, attempt| (step.to_string(), attempt, None);
    assert_eq!(
        executions,
        [
            plain("build", 1),
            plain("flaky", 1),
            plain("flaky", 2),
            plain("lint", 1),
            pick("slow"),
            pick("fast"),
            plain("tolerant", 1),
            // The second run executes only what did not hold.
            plain("lint", 1),
            plain("tolerant", 1),
        ]
    );
}
