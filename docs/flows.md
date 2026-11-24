# Writing flows

A flow is a TOML file, `flot.toml` by default. A step declares what it runs, what it reads, what it asserts,
and how that assertion is proved. A key flot does not know is an error, not
ignored: a misspelled `verify` or `inputs` would otherwise silently weaken a
gate.

```toml
[flow]
name = "release-candidate"
budget = { timeout_secs = 900, max_inline_bytes = 4096 }

[[step]]
id = "implement"
run = "gjc -p 'implement .flot-work/spec.md'"
needs = ["spec"]
inputs = [".flot-work/spec.md"]
outputs = ["src/**/*.rs"]
claims = ["the workspace still compiles after the agent's edits"]
verify = "cargo build --all-targets --quiet"
```

## Step keys

Digest over the command, the declared environment, the content of
every declared input file and proof input, the verify command, the declared
output patterns, and the receipt ids of upstream steps. Equal key ⇒ the same work over the same facts ⇒ the receipt is reused.
Declaring inputs is what makes a cache hit trustworthy; a step with none carries
a limitation saying so, in `flot plan` and in its receipt.

Input and proof-input patterns are globs relative to the flow file (`*` stays
within a directory, `**` crosses them). A wildcard skips what git ignores
(`.gitignore` files, `.git/info/exclude`, global excludes), so `**/*` means the
project's sources, not `target/` or `node_modules/`; it never enters `.git` or
`.flot`. A path the pattern spells out is always followed: `target/gen/*.rs`
and `.env` are inputs even when git ignores them. Output patterns reach ignored
files too, because products usually are ignored build output.

## Receipts

Split in two. `identity` is deterministic — command, step key,
status, exit code, output digests, inputs, produced files, dependency receipts, claim verdicts,
limitations, deriving tool. The receipt id *is* the digest of that body, so an
edited receipt stops authenticating. `observed` carries timing, host and bounded
inline previews, and is never hashed — two honest runs disagree on milliseconds,
not on facts.

## Claims

Three states: `verified` (a proof command ran after the step
succeeded and exited zero), `refuted` (it ran and rejected), `unverified`
(anything else — no proof command, step failed, proof timed out or was killed
by a signal, or a file the proof judges changed while it ran). Each one
records the proof command, its exit code and a digest of its output.
`allow_failure = true` lets a run continue past a failed step, but proves
nothing: that step's claims stay unverified.

## Verdict

`failed` if any step did not reach `ok` or any claim was refuted;
`unverified` if any claim lacks proof; `passed` only when every step succeeded
and every claim is proven.

## Attempts

`attempts = N` lets one run execute a step up to N times until it holds: it
succeeds and every claim with a proof command is verified. The default is 1.
This is for agent steps whose first try may not pass the proof:

```toml
[[step]]
id = "implement"
attempts = 2
run = """
prompt="Implement SPEC.md as slug.py."
if [ -n "${FLOT_FEEDBACK:-}" ]; then
  prompt="$prompt Your previous attempt did not pass: $(cat "$FLOT_FEEDBACK")"
fi
claude -p "$prompt" ...
"""
```

Each attempt gets `$FLOT_ATTEMPT` and `$FLOT_ATTEMPTS`. Every attempt after
the first also gets `$FLOT_FEEDBACK`, the path of a file that says why the
previous one did not hold, read back from its receipt: how it ended, each
claim's verdict and reason, its limitations, and the end of what its proof and
its own command printed (8 KiB of each). An attempt sees the workspace the
previous one left, so it fixes rather than starts over.

The step's receipt is the last attempt's. Earlier attempts are kept: the
manifest lists their receipts and observations, `flot show` names them,
`flot verify`, `bundle`, `import` and `gc` treat them like any other evidence,
and what they spent counts toward the run's spend and its cost ceiling. A run
that needed more than one says so:

```
ran      implement            ok        20556ms  claims 1v/0u/0r  receipt d78f48e273af  attempt 2
...
  - implement: took 2 attempts of 2 allowed: 1 refuted, 2 held
```

No further attempt starts once flot is asked to stop, once the cost ceiling is
reached (the run says which attempt was not run), or after a command that
could not start at all. Like a budget, `attempts` is not part of the step key:
work that held is the same work whichever attempt it took, and is reused on
the next run.

## Candidates

`candidates = [...]` runs one step several ways and keeps the best that holds.
Each candidate is the same step with `$FLOT_CANDIDATE` set to its name, so the
command chooses the agent, the model or the prompt:

```toml
[[step]]
id = "implement"
candidates = ["claude", "codex"]
run = """
case "$FLOT_CANDIDATE" in
claude) claude -p "Implement SPEC.md as slug.py." ... ;;
codex)  codex exec "Implement SPEC.md as slug.py." ... ;;
esac
"""
outputs = ["slug.py"]
claims = ["slug.py satisfies the acceptance check"]
verify = "python3 check_slug.py"
```

Candidates run one after another, each in its own copy of the project inside
the store (`.flot/tmp`): every file git does not ignore, plus every file the
step names as an input, proof input or output. The copy has no `.git`, and git
is told not to look above it (`GIT_CEILING_DIRECTORIES`), so what a candidate
writes and the git commands it runs stay in its copy; like every step, it is
not sandboxed, and a command that names a path outside the copy reaches it.
The copy holds the project as it is, outputs included, so a command that must
start without an earlier output removes it first. Each candidate gets its own receipt, and its own attempts when the
step has `attempts`.

The kept candidate is the first one in this order: one that held before one
that did not; among those that held, a known cost before an unknown one, and a
lower cost before a higher one; then the order the step names them in. A cost
is known only when every execution of the candidate reported `cost_usd`
through `$FLOT_METRICS`; a report of tokens alone, or an attempt that reported
nothing, makes it unknown, not free.
The kept candidate's outputs are written into the project, and its receipt is
the step's, so later steps see what was proven. Files a candidate deleted, or
wrote without declaring them as outputs, stay in its copy.

The run says how each candidate did and which one it kept:

```
  - implement: candidates claude held ($0.098488), codex held (cost unknown); kept claude
```

The manifest lists the candidates not kept, `flot show` names them,
`flot log STEP --candidate NAME` prints what one printed, and `verify`,
`bundle`, `import` and `gc` treat their receipts like any other evidence.
`verify` also checks that the kept candidate ranks first by this rule. What
every candidate spent counts toward the run, and no candidate starts once the
cost ceiling is reached. On the next run the best candidate whose receipt
still holds is reused, and none runs again; that includes a candidate added
since, so `flot run --force` runs them all. A candidate's name is part of its
step key; the list of candidates is not, so editing the list keeps every
candidate's receipt.

## Budgets

A budget is set in `[flow]` for every step, or per step, and a step's own
value wins.

| Field | Default | Kind |
|---|---|---|
| `timeout_secs` | 900 | admission limit, for the step and for its proof separately |
| `max_output_bytes` | 268435456 (256 MiB) per stream | admission limit |
| `max_inline_bytes` | 4096 | delivery budget |

Admission limits stop work and report it, never as a success: a step that hits
one is `timeout` or `failed`, and a proof that hits one leaves its claims
unverified. A delivery budget only trims what travels inline in receipts and
replies: the preview keeps the first quarter and the last three quarters of the
budget, cut at line breaks, because builds and test runners report what failed
at the end. The complete stream stays retrievable with `flot log STEP` (MCP
`flot_step_output`).

A budget is not part of the step key: a receipt that succeeded under a larger
budget is still reused after the budget shrinks.

## Concurrency

`flot run --jobs N`, or `max_parallel = N` in `[flow]`, runs up
to N independent steps at once; a step still waits for its dependencies. Because
completion order never enters a receipt or the run root, `--jobs 8` and
`--jobs 1` produce byte-identical digests for the same work — verified by test.
Concurrency is opt-in and the manifest records it as a limitation: flot checks
the files steps *declare* as outputs (see [Outputs](#outputs)), but cannot see files they
write without declaring them. A failing branch cancels only its own dependents; an
independent branch still completes.

Measured on a fan-out of three independent one-second steps joined by a gate:

```
flot run             3097ms   root sha256:d7b7703db719...
flot run --jobs 3    1066ms   root sha256:d7b7703db719...
```

The 2.9x is bounded by the three sleeps, and the root is unchanged because
wall-clock never enters a receipt.

## Execution

`run` and `verify` go through `sh -e -c` from the flow root on every platform
(on Windows, the `sh` of Git for Windows: on `PATH`, else beside `git.exe`), so
the first failing line of a multi-line command fails the step; without `-e` an
agent call that failed, followed by a line that succeeded, would be recorded as
success. Write `|| true` where a failure is expected. A step is one unit of
work: anything it left running in its process group when it exits is stopped,
and reported. A process that escapes the group (`setsid`, a daemon) cannot hold
the run open either; flot stops reading its output after a short grace period
and says the captured output may be incomplete, but it keeps running. On
Windows the step runs in a job object, which nothing it starts can leave, so
everything left behind is stopped. Files the
proof judges are re-read after the proof, so such a process cannot change them
unnoticed. Ctrl-C (or SIGTERM, SIGHUP; on Windows, Ctrl-C or closing the
console) stops the running steps with their process groups and still records
the run; a second one exits at once.

Steps are not sandboxed. A step runs with your user's permissions and
environment and can read or write anything you can, the `.flot` store included.
flot records and judges what steps did; it does not confine them. Run flows
you have read.

A step that cannot run at all, say because its command cannot start, gets a
`failed` receipt saying why, and the run still closes with a manifest that
records what earlier steps spent. Only a store that cannot record receipts
stops a run without one.

## Cancellation

A step whose dependency failed, or succeeded with claims a
proof refuted, is never started; it gets a `cancelled` receipt naming the
dependency and why. Refuted work already fails the run, so building on it could
only spend time and money. An *unproven* dependency does not block: its claims
are unknown, not false.

## Guarded proofs

`verify_inputs` names the files a proof reads, such as an
acceptance test. Their content is part of the step key, so tightening the test
re-proves the claim instead of reusing a verdict the new test never gave. They
are also re-read after the step runs: a step that edited its own proof gets
unverified claims and the proof is not run. A step that edits its declared
`inputs` is reported too, since its receipt then describes files that no longer
exist.

That catches a step that edits its proof while it runs. It does not catch an
agent that edits the proof before the run, or commits the weaker check: the
run then proves the weaker check, faithfully.

### Proofs from a revision

`proofs_from` names a git revision the agent does not control, such as
`origin/main`. Each step's proof must be as it is there: its claims, its
`verify` command, its `verify_inputs` patterns, and the files they match. A
step whose proof differs still runs, but its proof does not, its claims stay
unverified, and no receipt stands in for it. A claim dropped from the flow is
still listed, and a step with claims at the revision that the flow no longer
has is named.

```toml
[flow]
name = "acceptance"
proofs_from = "origin/main"

[[step]]
id = "implement"
run = "python3 make.py"
inputs = ["make.py"]
claims = ["the acceptance check passes"]
verify = "python3 tests/acceptance.py"
verify_inputs = ["tests/**"]
```

After `tests/acceptance.py` is weakened to `assert True` in the working copy
(real output, the store's ids as printed):

```text
$ flot run
ran      implement            ok        11ms  claims 0v/1u/0r  receipt d465823b6a30

UNVERIFIED 1795531484579-cb8b58c66018  root cb8b58c66018  31ms
limitations:
  - implement: the proof differs from origin/main (df9bf14a7787) (tests/acceptance.py differs); claims stay unverified and the proof was not run
  - proofs checked against origin/main (df9bf14a7787)
```

The revision is the one `--proofs-from REV` names, else `proofs_from` in the
flow as committed at `HEAD`, else in the working copy: deleting the line from
the working copy does not turn the check off. A branch that commits both a
weaker check and its own `proofs_from` does pass its own run, so
`flot verify --proofs-from origin/main` holds a recorded run to a revision
too, and exits 2 when a proof differs:

```text
$ flot verify --proofs-from origin/main
run 1795531484704-4498c0026bef  receipts checked 1  root 4498c0026bef
integrity: intact
verdict passed
proofs: NOT as at origin/main (df9bf14a7787)
  - implement: tests/acceptance.py differs
authorship: unsigned
```

A receipt does not record the `verify_inputs` patterns, so `verify` compares
the files they matched, the claims and the command. The revision must be a
commit in the repository (`fetch` it first in CI); one that is not is an
environment error. The MCP tools take no revision: an agent cannot pick what
its own proofs are checked against. Files are compared byte for byte, so a
checkout that rewrites line endings differs from the revision.

### Refusing the edit

`flot guard PATH...` names the paths whose change would touch the proofs: the
flow file, a file some step's `verify_inputs` matches, and the store. It exits
2 when there is one. Each path is checked against the flow of the current
directory and the flow nearest the path.

```text
$ flot guard tests/acceptance.py flot.toml
protected  tests/acceptance.py: the proof of step `implement` reads it
protected  flot.toml: it is the flow file, which says what proves the work
```

`flot guard --hook` reads a Claude Code `PreToolUse` payload on stdin and
answers with a denial; the [Claude Code plugin](mcp.md#claude-code-plugin)
installs it for `Edit`, `Write`, `MultiEdit` and `NotebookEdit`. A shell
command such as `sed -i` is not a file edit the hook sees; `proofs_from` is
what catches that.

## Scope

`may_change` lists, as globs, the files a step may create, edit or delete. A
step that changes any other file fails and the limitation names each one.
Its `outputs` are in scope too. What git ignores, `.git` and the store are
not looked at, so build trees and caches do not count, and a write there is
not caught.

```toml
[[step]]
id = "fix"
run = "sh agent.sh"
inputs = ["agent.sh"]
may_change = ["src/**"]
claims = ["the slug function is fixed"]
verify = "sh check.sh"
verify_inputs = ["check.sh"]
```

Here `agent.sh` fixes `src/slug.txt` and also rewrites the check (real
output):

```text
$ flot run
ran      fix                  failed    12ms  claims 0v/1u/0r  receipt 67a4796ebf56

FAILED 1795531811505-1a507846ea6c  root 1a507846ea6c  29ms
limitations:
  - fix: changed files outside may_change: check.sh
  - fix: claims are unverified because the step did not succeed
```

Every file the scan reaches is hashed before and after the step, so the check
costs a read of the project per execution. A step with `may_change` runs
alone: no other step of the run runs beside it, so what changed is its own
doing (its proof, and candidates in their own copies, are not held back). The
scope is part of the step key, so narrowing it runs the step again. A change
of mode alone is not a change of content and is not reported.

## Outputs

`outputs` names the files a step produces. When the step finishes
their content goes into the store and the receipt, and a success that produced
no file for a declared pattern is `failed`: it did not do what the flow says.
Reusing a receipt puts missing outputs back, so the files on disk are the ones
its claims were proven against. An output that exists with different content
is never overwritten, and never vouched for by a receipt that proved other
content: the receipt is not reused and the step runs again, so its proof judges
the file that is actually there. Output files travel in a bundle, so an
importing host gets the produced artifact, not just a report about it. Two steps that
produce the same file without one needing the other are reported, because which
content survives depends on scheduling. Patterns must stay inside the flow root,
a step cannot declare its own proof as an output, and a receipt is never reused
if it names a file the local flow does not declare for that step. A file reached
through a symbolic link is neither recorded as an output nor restored through:
the link could point anywhere. Outputs are what the patterns match
when the step finishes, so a stale file left by an earlier run is recorded too;
keep patterns as specific as the step's real products.
