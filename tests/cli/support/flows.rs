//! Flows that tests of several topics run.

pub(crate) const PROVEN: &str = r#"
[flow]
name = "proven"

[[step]]
id = "write"
run = "echo flot > artifact.txt"
inputs = ["seed.txt"]
claims = ["artifact.txt contains flot"]
verify = "grep -q flot artifact.txt"
"#;

pub(crate) const TWO_STEPS: &str = r#"
[flow]
name = "two"

[[step]]
id = "a"
run = "echo first"

[[step]]
id = "b"
run = "echo second"
"#;

/// A stand-in for an agent: each attempt adds one to `count`, prints what
/// flot told it, and the proof wants `count` to reach `GOAL`.
pub(crate) const RETRYING: &str = r#"
[flow]
name = "retrying"

[[step]]
id = "fix"
attempts = 3
run = """
echo "attempt $FLOT_ATTEMPT of $FLOT_ATTEMPTS"
if [ -n "${FLOT_FEEDBACK:-}" ]; then cat "$FLOT_FEEDBACK"; fi
n=$(cat count 2>/dev/null || echo 0); echo $((n+1)) > count
"""
inputs = ["flot.toml"]
outputs = ["count"]
claims = ["count reached the goal"]
verify = "echo count is $(cat count); test $(cat count) -ge GOAL"
"#;

/// Three ways to answer: one wrong, two right at different prices. Each also
/// leaves a stray file, which must stay in its own workspace.
pub(crate) const CANDIDATES: &str = r#"
[flow]
name = "candidates"

[[step]]
id = "solve"
candidates = ["wrong", "pricey", "cheap"]
run = """
touch "stray-$FLOT_CANDIDATE"
case "$FLOT_CANDIDATE" in
  wrong)  echo 41 > answer ;;
  pricey) echo 42 > answer; echo '{"cost_usd": 0.5}' > "$FLOT_METRICS" ;;
  cheap)  echo 42 > answer; echo '{"cost_usd": 0.1}' > "$FLOT_METRICS" ;;
esac
echo "$FLOT_CANDIDATE read $(cat question)"
"""
inputs = ["question"]
outputs = ["answer"]
claims = ["the answer is 42"]
verify = "test $(cat answer) = 42"

[[step]]
id = "use"
needs = ["solve"]
inputs = ["answer"]
run = "echo using $(cat answer)"
"#;

pub(crate) const ANCHORED: &str = r#"
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

pub(crate) const CONFINED: &str = r#"
[flow]
name = "confined"

[[step]]
id = "inside"
run = "echo a > src/a.txt; ln src/a.txt src/linked.txt; echo t > \"$TMPDIR/t\"; echo n > /dev/null"
sandbox = { write = ["src"] }
claims = ["src/a.txt was written"]
verify = "grep -q a src/a.txt"

[[step]]
id = "outside"
run = "ln flot.toml src/flot.toml || echo link refused; echo b > b.txt"
sandbox = { write = ["src"] }

[[step]]
id = "offline"
run = "if nc -z 127.0.0.1 PORT; then echo online; exit 1; fi"
sandbox = { network = false }

[[step]]
id = "online"
run = "nc -z 127.0.0.1 PORT"
"#;
