//! The progress a run tells on stderr while its steps run.

use crate::support::{flot_bin, Project};
use std::process::Command;

/// Steps that print as they run: `build` prints three lines, the last with
/// no line ending, and its proof one; `lint` prints to stderr and fails;
/// `ship`, which needs it, is not run.
const PRINTING: &str = r#"
[flow]
name = "printing"

[[step]]
id = "build"
run = "echo one; echo two; printf three"
claims = ["it built"]
verify = "echo proven"

[[step]]
id = "lint"
run = "echo bad >&2; exit 1"

[[step]]
id = "ship"
needs = ["lint"]
run = "true"
"#;

/// `text` with each duration, such as `1.2s`, written `T`.
fn timeless(text: &str) -> String {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    // `1.2s`, or `42ms` under a tenth of a second.
    let duration = |word: &str| {
        word.strip_suffix("ms")
            .is_some_and(|millis| digits(millis) && millis.len() <= 2)
            || word
                .strip_suffix('s')
                .and_then(|number| number.split_once('.'))
                .is_some_and(|(whole, tenth)| digits(whole) && tenth.len() == 1 && digits(tenth))
    };
    text.lines()
        .map(|line| {
            let words: Vec<&str> = line
                .split(' ')
                .map(|word| if duration(word) { "T" } else { word })
                .collect();
            format!("{}\n", words.join(" "))
        })
        .collect()
}

#[test]
fn run_tells_each_step_on_stderr_as_it_runs() {
    let project = Project::new(PRINTING);
    let first = project.flot(&["run", "--progress", "plain"]);
    assert_eq!(first.code, 1, "{}", first.stderr);
    assert_eq!(
        timeless(&first.stderr),
        concat!(
            "run 3 steps, 1 job\n",
            "start build\n",
            "[build] one\n",
            "[build] two\n",
            "[build] three\n",
            "[build:proof] proven\n",
            "done build passed T claims 1/1\n",
            "start lint\n",
            "[lint:stderr] bad\n",
            "done lint failed T\n",
            "done ship not run: dependency lint (failed) did not hold\n",
            "run finished T\n",
        )
    );
    // A reused step prints nothing; a failed one is never reused.
    let again = project.flot(&["run", "--progress", "plain"]);
    let reused = concat!(
        "run 3 steps, 1 job\n",
        "cached build passed claims 1/1\n",
        "start lint\n",
        "[lint:stderr] bad\n",
        "done lint failed T\n",
        "done ship not run: dependency lint (failed) did not hold\n",
        "run finished T\n",
    );
    assert_eq!(timeless(&again.stderr), reused);
    // Off a terminal, as here, `auto` is plain, whatever the terminal type.
    for term in ["xterm-256color", "dumb"] {
        let auto = Command::new(flot_bin())
            .arg("run")
            .current_dir(project.path())
            .env("FLOT_SIGNING_KEY", project.key_path())
            .env("TERM", term)
            .output()
            .unwrap();
        assert_eq!(timeless(&String::from_utf8_lossy(&auto.stderr)), reused);
    }
    let off = project.flot(&["run", "--progress", "off"]);
    assert_eq!((off.code, off.stderr.as_str()), (1, ""));
}

/// Twin projects run the same flow, one telling its progress and one not:
/// what each prints on stdout, as text or as JSON, and its exit code are the
/// same, but for the times and the run id's moment.
#[test]
fn progress_leaves_stdout_json_and_the_exit_code_as_they_were() {
    // Runs of digits, which are what times are made of, read as one `0`.
    let digitless = |text: &str| {
        let mut out = String::new();
        for c in text.chars() {
            if !c.is_ascii_digit() {
                out.push(c);
            } else if !out.ends_with('#') {
                out.push('#');
            }
        }
        out
    };
    fn timeless_json(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for key in ["run_id", "started_at_ms", "duration_ms", "observed_digest"] {
                    map.remove(key);
                }
                map.values_mut().for_each(timeless_json);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(timeless_json),
            _ => {}
        }
    }
    let twins = |args: &[&str]| {
        let told = Project::new(PRINTING).flot(&[args, &["--progress", "plain"]].concat());
        let quiet = Project::new(PRINTING).flot(&[args, &["--progress", "off"]].concat());
        assert_eq!(told.code, quiet.code);
        assert!(told.stderr.contains("[build] one\n"), "{}", told.stderr);
        assert_eq!(quiet.stderr, "");
        (told, quiet)
    };

    let (told, quiet) = twins(&["run"]);
    assert!(!told.stdout.contains("[build]"), "{}", told.stdout);
    assert_eq!(digitless(&told.stdout), digitless(&quiet.stdout));

    let (told, quiet) = twins(&["--json", "run"]);
    let (mut told, mut quiet) = (told.json(), quiet.json());
    assert_eq!(told["result"], "failed");
    timeless_json(&mut told);
    timeless_json(&mut quiet);
    assert_eq!(told, quiet);
}

/// Steps running at once print their lines in turns, each line whole.
#[test]
fn parallel_steps_never_split_a_line() {
    let count = |name: &str| {
        format!(
            "[[step]]\nid = \"{name}\"\nrun = 'i=0; while [ $i -lt 300 ]; do echo \"{name} says line $i of many\"; i=$((i+1)); done'\n\n"
        )
    };
    let project = Project::new(&format!(
        "[flow]\nname = \"chatty\"\n\n{}{}",
        count("left"),
        count("right")
    ));
    let run = project.flot(&["run", "-j", "2", "--progress", "plain"]);
    run.ok();
    for name in ["left", "right"] {
        let lines: Vec<&str> = run
            .stderr
            .lines()
            .filter(|line| line.starts_with(&format!("[{name}]")))
            .collect();
        let expected: Vec<String> = (0..300)
            .map(|i| format!("[{name}] {name} says line {i} of many"))
            .collect();
        assert_eq!(lines, expected);
    }
    let others: Vec<&str> = run
        .stderr
        .lines()
        .filter(|line| !line.starts_with("[left] ") && !line.starts_with("[right] "))
        .collect();
    assert_eq!(others.len(), 6, "{others:?}");
}

/// The live view, asked for where a terminal is not: every row it draws for
/// a running step it takes back with cursor-up and clear-line alone, and
/// what steps print and how they ended stays.
#[cfg(unix)]
#[test]
fn the_live_view_takes_back_every_row_it_draws() {
    // `build` takes long enough to be drawn running.
    let project = Project::new(&PRINTING.replace("echo two;", "echo two; sleep 0.5;"));
    let output = Command::new(flot_bin())
        .args(["run", "--progress", "live"])
        .current_dir(project.path())
        .env("FLOT_SIGNING_KEY", project.key_path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let erased = stderr.matches("\x1b[A\r\x1b[K").count();
    let drawn = stderr
        .split("\x1b[A\r\x1b[K")
        .flat_map(str::lines)
        .filter(|line| line.contains(" running "))
        .count();
    assert!(erased > 0, "{stderr:?}");
    assert_eq!(erased, drawn, "{stderr:?}");
    assert_eq!(stderr.matches('\x1b').count(), 2 * erased, "{stderr:?}");
    let shown: String = stderr.split("\x1b[A\r\x1b[K").collect();
    let kept: Vec<&str> = shown
        .lines()
        .filter(|line| !line.contains(" running "))
        .collect();
    assert_eq!(
        timeless(&kept.join("\n")),
        concat!(
            "[build] one\n",
            "[build] two\n",
            "[build] three\n",
            "[build:proof] proven\n",
            "\u{2713} build passed T claims 1/1\n",
            "[lint:stderr] bad\n",
            "\u{2717} lint failed T\n",
            "\u{2717} ship not run: dependency lint (failed) did not hold\n",
        )
    );
}
