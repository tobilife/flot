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
run = "claude -p 'implement .flot-work/spec.md'"
needs = ["spec"]
inputs = [".flot-work/spec.md"]
outputs = ["src/**/*.rs"]
claims = ["the workspace still compiles after the agent's edits"]
verify = "cargo build --all-targets --quiet"
```

`run` is a shell command, so the agent is whichever command-line agent you
run headless: `claude -p`, `codex exec`, or any other. flot judges what the
step leaves behind through `verify`, not which agent or harness made it.

## Step keys

Digest over the command, the declared environment, the content and
executable bit of every declared input file and proof input, the verify command, the declared
output patterns, and the receipt ids of upstream steps. Equal key ⇒ the same work over the same facts ⇒ the receipt is reused,
if the step exited zero and the files it produced and changed are still as it
left them.
Declaring inputs is what makes a cache hit trustworthy; a step with none carries
a limitation saying so, in `flot plan` and in its receipt.

The rest of the environment is not keyed: variables the step does not declare
in `env`, `PATH`, and the compilers and tools found on it. Upgrading a
compiler does not run a step again by itself; a step that
[always runs](#steps-that-always-run) can probe it.

Input and proof-input patterns are globs relative to the flow file (`*` stays
within a directory, `**` crosses them). A wildcard skips what git ignores
(`.gitignore` files, `.git/info/exclude`, global excludes), so `**/*` means the
project's sources, not `target/` or `node_modules/`; it never enters `.git` or
`.flot`. A path the pattern spells out is always followed: `target/gen/*.rs`
and `.env` are inputs even when git ignores them. Output patterns reach ignored
files too, because products usually are ignored build output.

Where git does not trust the file system's executable bit (`core.fileMode`
is false: Git for Windows, and WSL's `/mnt` drives, where every file reads as
executable), an input git tracks is keyed by the bit git records, the one a
Linux or macOS checkout has. Without that, a key computed on such a drive
never matched one computed in CI, so a run signed there could not be reused
there. A file git does not track, and an output, keep the bit the file system
shows.

### Files a step changed

A step also answers for the files it changes. flot reads the project before
and after each step, as [`may_change`](#scope) does, and the receipt records
every file the step created, edited or deleted other than its outputs, with
the digest of what it left (or that it deleted the file). Its claims were
proven on those files, so the receipt is reused only while each one is still
so; once someone edits one, or brings back a file the step deleted, the step
runs again:

```text
$ echo old > src/lib.txt
$ flot run
ran      edit                 ok        12ms  claims 1v/0u/0r  receipt 46ce815e7705

PASSED 1795655372863-685b56d208be  root 685b56d208be  55ms
limitations:
  - edit: src/lib.txt changed since its receipt; the step runs again
  - proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)
```

Here the step writes `new` into `src/lib.txt` and its proof checks for it.
Up to 0.2.6 the edit was answered with the cached receipt and `PASSED`,
while the proof failed. The re-run seals the receipt it sealed before the
edit. A file an earlier receipt of the step records, and that is still as
that receipt says, is recorded again when a run finds it already as the step
leaves it, such as a file the step deletes that is already gone. A step that
changes nothing records nothing, and its receipt is what it always was.

The cost is re-runs. An agent step that edited `src/**` runs again, and is
paid for again, after a person edits any file it changed, even a line its
claims do not depend on: flot cannot tell which edits matter to a proof, and
a proof that judged other content says nothing about this one. What git
ignores, `.git` and the store are not read, so a product there, such as a
file under `.flot-work/`, is checked only if the step declares it in
`outputs`. Across steps that run at once see [Concurrency](#concurrency).

### Steps that always run

A step with `always = true` runs on every run and is never reused, so it
can probe what no key covers:

```toml
[[step]]
id = "toolchain"
run = "rustc -vV && cargo -V"
always = true

[[step]]
id = "build"
run = "cargo build"
needs = ["toolchain"]
```

Its receipt id follows what it printed and produced, so it comes out the same
while the tools do, and the steps that need it are reused; once they print
something else, those steps run again. Real output, for a probe that prints
the version in `ver`: the second run, and one after the version changed
(the verdict and limitations are left out):

```text
$ flot run
ran      toolchain            ok        11ms  claims 0v/0u/0r  receipt f42699e3dc2d
    | 1.88
cached   build                ok        11ms  claims 1v/0u/0r  receipt c62bdb4f366e

$ echo 1.85 > ver
$ flot run
ran      toolchain            ok        11ms  claims 0v/0u/0r  receipt 05a7fec7721d
    | 1.85
ran      build                ok        11ms  claims 1v/0u/0r  receipt c3d387e28bce
    | built
```

`flot plan` runs nothing, so it cannot know what the probe will print. It
predicts on what the probe recorded last time and says so on each step that
prediction makes reused; in `--json` the step lists the probes it rests on in
`assumes`. Real output, after one run:

```text
$ flot plan
flow probed (a349531adac2)
  run   toolchain            inputs 0   claims 0   no claims
        ! runs on every run (always); the steps that need it are reused only when its receipt comes out the same, which running it decides
        ! step declares no claims; its success proves nothing
  reuse build                inputs 1   claims 1   verify `true`
        ? reused if toolchain comes out as last time
```

`always` is not part of the step key. It is refused with `candidates`: every
candidate would run on every run, and which one is kept could change with what
each reported spending. With `fails_before`, the proof runs before the change
on every run too.

## Receipts

Split in two. `identity` is deterministic — command, step key,
status, exit code, output digests, inputs, produced files, other files the step changed, dependency receipts, claim verdicts,
limitations, deriving tool. The receipt id *is* the digest of that body, so an
edited receipt stops authenticating. `observed` carries timing, host and bounded
inline previews, and is never hashed — two honest runs disagree on milliseconds,
not on facts.

## Claims

Three states: `verified` (a proof command ran after the step
succeeded and exited zero), `refuted` (it ran and rejected), `unverified`
(anything else — no proof command, step failed, proof timed out or was killed
by a signal, a file the proof judges changed while it ran, or, on Linux, a
process the step or its proof started left its process group; see
[Execution](#execution)). Each one
records the proof command, its exit code and a digest of its output.
`allow_failure = true` lets a run continue past a failed step, but proves
nothing: that step's claims stay unverified. Its receipt is `ok` but records
the exit code, and like any failure it is never reused: the next run tries
the step again.

### Proofs that change their mind

A proof run twice over the same step and the same files must say the same
thing; one that does depends on something the flow does not declare (time,
the network, the machine, chance), and a pass from it proves little.
`flot flaky` reads the newest runs executed here (`--runs N`, default 50;
imported runs ran elsewhere and are skipped) and names each step that was
`ok` and `failed` on one key, and each proof that was `verified` and
`refuted` over one step key and one set of proof files. It exits 2 when there
is one, and `flot doctor` warns. A claim left unverified because its step
failed is the step's flakiness, not the proof's. Real output, for a proof
that passes on even seconds, after six `flot run --force`:

```text
$ flot flaky
proof of "the checkout page loads" (step `e2e`, key f8e6bec8c477) over the same files:
  refuted   in 4 runs, newest 1795561545037-15d92f3670c1
  verified  in 2 runs, newest 1795561544365-388edc142298
0 step(s) and 1 proof(s) gave different results on the same inputs in 6 run(s) read
```

A success is reused rather than run again, so a flaky proof shows up only
across runs that executed it: after `--force`, on another branch's inputs
and back, or where a failure made the step run again.

## Verdict

`failed` if any step did not reach `ok` or any claim was refuted;
`unverified` if any claim lacks proof; `passed` only when every step succeeded
and every claim is proven.

A run of only some of the flow's steps (`flot run --only`, MCP `only`) says
nothing of the steps it left out, so it is not a pass of the flow: when the
steps it ran passed, it is `partial`, and exits 2. A selected step that
fails still makes it `failed`. The run records its selection (`partial`:
how many steps it `selected`, `of` how many), which a signature covers; a
run of every step records none. Real output, for a flow whose `test` step
fails:

```text
$ flot run --only lint
ran      lint                 ok        11ms  claims 1v/0u/0r  receipt 86cca38b2e28

PARTIAL 1795654286450-8366567618d3  root 8366567618d3  55ms
limitations:
  - proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)
  - partial run: 1 of 2 steps selected
$ flot verify
run 1795654286450-8366567618d3  receipts checked 1  root 8366567618d3
integrity: intact
verdict partial (1 of 2 steps selected, and they passed)
authorship: unsigned
```

`flot run --json`, `flot verify --json` and the MCP tools `flot_run` and
`flot_verify` state the judgment as `result`, the one the
[exit code](cli.md#exit-codes) gives. It is the worst that holds, in this
order: `broken` (from `verify`: the evidence does not authenticate),
`failed`, `unverified` (including a run whose proofs differ from the
revision `verify` holds it to), `partial`, `passed`. The recorded `verdict`
is the receipts' own: `passed` for a partial run whose steps passed, and
whatever an altered record says for a broken one. `flot runs` lists that
recorded verdict.

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
step has `attempts`. A step with candidates cannot set
[`always`](#steps-that-always-run).

The kept candidate is the first one in this order: one that held before one
that did not; among those that held, a known cost before an unknown one, and a
lower cost before a higher one; then the order the step names them in. A cost
is known only when every execution of the candidate reported `cost_usd`
through `$FLOT_METRICS`; a report of tokens alone, or an attempt that reported
nothing, makes it unknown, not free.
The kept candidate's outputs are written into the project, and its receipt is
the step's, so later steps see what was proven. Files a candidate deleted, or
wrote without declaring them as outputs, stay in its copy, so its receipt
answers only for its outputs.

Patterns are matched in the copy by the project's ignore rules, not by the
store's (`.flot/.gitignore` ignores everything beneath it), so a wildcard
input or proof input finds in the copy what it finds in the project. A
candidate that rewrites a proof file a wildcard names is caught like any other
step: `the step changed its own proof (checks/check.sh)`, and its claims stay
unverified.

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
`--jobs 1` produce byte-identical digests for the same work — verified by test —
as long as no step changes undeclared files while another runs.
Concurrency is opt-in and the manifest records it as a limitation: flot checks
the files steps *declare* as outputs (see [Outputs](#outputs)), but cannot tell
which of the steps running at once changed a file they do not declare. Such a
file is recorded in the receipt of every step that was running when it changed
(see [Files a step changed](#files-a-step-changed)). That only makes those
steps run again once the file changes, never reuses one wrongly, but the
receipt ids of steps that run beside an editing step then depend on timing. A
step with `may_change` runs alone, so what it records is its own. A failing
branch cancels only its own dependents; an independent branch still completes.

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
and reported. On Linux and macOS a process that escapes the group
(`setsid`, a daemon) is stopped too. On Linux flot adopts every process
orphaned below it; macOS cannot adopt, so there flot looks every 20 ms at the
processes below each running step command and proof and remembers them, so
one that left its group is still known after its parent has exited. After
each step command and each proof, flot stops every such process that left
its group, with any group that process leads. The step's verified
claims then stay unverified, since the process may have changed files after
the proof judged them; a refuted claim stays refuted. Real output, for a
step that writes `out.txt` and leaves behind a process that would rewrite it
a second later:

```text
$ flot run
ran      work                 ok        23ms  claims 0v/1u/0r  receipt 493883b23970

UNVERIFIED 1795654346706-ec7374f98a1c  root ec7374f98a1c  61ms
limitations:
  - work: step declares no inputs; only its command and dependencies key it, so it is reused even when files it reads change
  - work: a process it started left its process group and was stopped (pid 2496231)
  - proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)
```

flot tells which step started such a process by `FLOT_COMMAND_ID` in its
environment, and on macOS also by the processes it saw below the step. On
Linux one that cleared its environment (`env -i`) is charged to every step
running when it was found; on macOS it is charged to the step it was seen
below. A daemon a tool starts on purpose (`sccache`, a Gradle or Bazel
server, a detached `git gc --auto`) is stopped the same way and leaves the
step unverified: start it outside the flow. On macOS a process that clears
its environment and leaves its parent within 20 ms, before flot has looked,
is never seen and keeps running; flot stops reading its output after a short
grace period and says the captured output may be incomplete.
On Windows the step runs in a job object, which nothing it starts can leave,
so everything left behind is stopped. Files the
proof judges are re-read after the proof, so a change made to them while it
runs is noticed. Ctrl-C (or SIGTERM, SIGHUP; on Windows, Ctrl-C or closing the
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
exist. `flot doctor` warns of a proof that declares no `verify_inputs`
(`proof of work reads no declared file, so an edit to it cannot be detected or guarded`),
on one line that names every such step; for this repository's own flow,
whose proofs are commands over the step's inputs:

```text
$ flot doctor
warn  proof inputs    proofs of format, build, lint, unit, cli, rules read no declared file, so an edit to them cannot be detected or guarded
```

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
ran      implement            ok        11ms  claims 0v/1u/0r  receipt 64a4434370f2

UNVERIFIED 1795655458543-1255e8d1c922  root 1255e8d1c922  39ms
limitations:
  - implement: the proof differs from origin/main (d5f8f8532d6b) (tests/acceptance.py differs); claims stay unverified and the proof was not run
  - proofs checked against origin/main (d5f8f8532d6b)
```

The revision is the one `--proofs-from REV` names, else `proofs_from` in the
flow as committed at `HEAD`, else in the working copy: deleting the line from
the working copy does not turn the check off. A branch that commits both a
weaker check and its own `proofs_from` does pass its own run, so
`flot verify --proofs-from origin/main` holds a recorded run to a revision
too, and exits 2 when a proof differs:

```text
$ flot verify --proofs-from origin/main
run 1795655463775-08e44043ae35  receipts checked 1  root 08e44043ae35
integrity: intact
verdict passed
proofs: NOT as at origin/main (d5f8f8532d6b)
  - implement: tests/acceptance.py differs
authorship: unsigned
```

Without a revision nothing holds a proof, and a run with one says so once.
It can still compare: when a step's proof (the files it read, its `verify`
command or its claims) differs from the newest earlier run of that step
executed here, the run names the change and keeps its verdict. Real output,
after `check.sh` was weakened to `exit 0` between two runs:

```text
$ flot run
ran      work                 ok        11ms  claims 1v/0u/0r  receipt b7d50a79d9a1

PASSED 1795654335319-46505608e0e3  root 46505608e0e3  60ms
limitations:
  - work: its proof changed since run 1795654335223-0a7ca4e8199a (check.sh); read the change before trusting the verdict
  - proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)
```

The weakened proof still passes; the note says where to look, and the next
run compares with this one. `flot init` in a clone whose `origin` names a
default branch sets `proofs_from` to it (`origin/main`), so the starter's
claims stay unverified until the flow is on that branch; elsewhere it writes
the setting commented out, with why.

### Changes no step read

Held to a revision, `flot verify` also lists the files under the flow's
directory that differ from it (edited, added or deleted, unless git ignores
them) and that no step of the run read as an input, checked as a proof
input or produced. No claim speaks for those changes, so they are where a
reviewer has to look. The flow file is left out (its proofs are compared
above), and an untracked directory none of whose files was read is named
once. It does not change the verdict or the exit code. Real output, after
`src/lib.sh` (an input) and `README.md` were edited and `deploy.conf` added:

```text
$ flot verify
run 1795561275154-f4546837d30f  receipts checked 1  root f4546837d30f
integrity: intact
verdict passed
proofs: as at main (97d175a0d3cc)
changes no step read: README.md, deploy.conf
authorship: unsigned
```

The files are those of the working tree when `verify` runs, which in CI is
the tree the run saw.

A receipt does not record the `verify_inputs` patterns, so `verify` compares
the files they matched, the claims and the command. The revision must be a
commit in the repository (`fetch` it first in CI); one that is not is an
environment error. The MCP tools take no revision: an agent cannot pick what
its own proofs are checked against. Files are compared byte for byte, so a
checkout that rewrites line endings differs from the revision.

### Proofs that fail before the change

A proof that passes before the change as well shows nothing the change did:
a test that was already green, or one too weak to fail. With
`fails_before = true`, once a step's proof passes it runs again on a copy of
the work, where it must pass too, and then there with every change since the
revision undone: edited and deleted files get their content and mode at the
revision back, new files are removed, and what git ignores is left as it is.
The claims verify only if the proof fails there. The copy holds what git
sees plus the step's declared inputs, proof inputs and outputs, and has no
`.git`, so the proof must run from such a checkout; one that does not pass on
the copy leaves the claims unverified rather than read a failure there as
evidence. It needs a revision (`proofs_from` or `--proofs-from`), claims and
a `verify` command, and is not available with `candidates`.

```toml
[flow]
name = "slug"
proofs_from = "main"

[[step]]
id = "fix"
run = "sh slug.sh 'Hello World'"
inputs = ["slug.sh"]
claims = ["slug.sh lowercases what it is given"]
verify = "sh tests/slug.sh"
verify_inputs = ["tests/**"]
fails_before = true

[[step]]
id = "docs"
run = "true"
inputs = ["README.md"]
claims = ["the README names slug.sh"]
verify = "grep -q slug.sh README.md"
verify_inputs = ["README.md"]
fails_before = true
```

Real output, after `slug.sh` was changed to lowercase; the README already
named it at `main`:

```text
$ flot run
ran      docs                 ok        11ms  claims 0v/1u/0r  receipt 74d47914d421
ran      fix                  ok        11ms  claims 1v/0u/0r  receipt c7d08429cb05
    | hello-world

UNVERIFIED 1795562469413-60af8505578c  root 60af8505578c  131ms
limitations:
  - docs: the proof passes with the changes undone to main (e023c8566969) too, so it shows nothing this change did; claims stay unverified
  - proofs checked against main (e023c8566969)

$ flot show latest
run 1795562469413-60af8505578c  flow slug  root 60af8505578c
  docs                 ok        ran     receipt 74d47914d421
      ? [unverified] the README names slug.sh
        reason: the proof passes with the changes undone to main (e023c8566969) too, so it shows nothing this change did
        before the change (e023c8566969): passed
  fix                  ok        ran     receipt c7d08429cb05
      + [verified] slug.sh lowercases what it is given
        before the change (e023c8566969): exited 1
verdict unverified  claims 1v/1u/0r  concurrency 1
limitations:
  - docs: the proof passes with the changes undone to main (e023c8566969) too, so it shows nothing this change did; claims stay unverified
  - proofs checked against main (e023c8566969)
```

The receipt keeps the run before the change as the claim's `before` record:
the commit, the exit code and the output. Both runs on the copy count toward
the step's spend. Whether a proof fails depends on the revision, so a cached
receipt is reused only if it was checked at the one the run is held to, and
`flot verify` treats a step that drops `fails_before`, or a verified claim
not checked at the revision, as a proof that differs.

### Refusing the edit

`flot guard PATH...` names the paths whose change would touch the proofs: the
flow file, a file some step's `verify_inputs` matches, and the store. It exits
2 when there is one.

```text
$ flot guard tests/acceptance.py flot.toml
protected  tests/acceptance.py: the proof of step `implement` reads it
protected  flot.toml: it is the flow file, which says what proves the work
```

A path is checked against every flow from the current directory, and from
the path, up to the top of the repository, so a `flot.toml` created below a
flow does not hide it; a new `flot.toml` inside a flow's project is itself
protected. A path is judged as named, then where it leads through symbolic
links, then as a hard link to a protected file (Linux and macOS), and a
directory by what it holds. On Windows and macOS names are compared ignoring
case. Where no flow is, nothing is refused. Real output:

```text
$ flot guard tests alias.sh
protected  tests: it holds tests/accept.sh: the proof of step `work` reads it
protected  alias.sh: it leads to check.sh: the proof of step `work` reads it
```

It also protects what decides whether the proofs are checked at all, in the
repository the flow is in (or the flow's directory outside one): Claude Code's
settings (`.claude/settings*.json` at any depth), which run the hook below,
CI workflows, which run the flow, git hooks and git's configuration, which
can point it at other hooks; and the user's own `~/.claude/settings*.json`
and Claude Code plugins (`~/.claude/plugins`), and the flot binary that
enforces the guard. In this repository (real output):

```text
$ flot guard .github/workflows/ci.yml .claude/settings.json .git/hooks/pre-push ~/.claude/settings.json src/lib.rs
protected  .github/workflows/ci.yml: it is a CI workflow, which decides what CI checks
protected  .claude/settings.json: it configures Claude Code, including the hook that enforces this guard
protected  .git/hooks/pre-push: it is a git hook, code git runs on its own
protected  ~/.claude/settings.json: it configures Claude Code for this user, including the hook that enforces this guard
```

An agent that is refused one of these can still ask you to make the change.
The hook stops an agent, not a pull request: to keep CI from being edited
away, make the job that runs flot a required check on the protected branch.

`flot guard --command CMD` reads a shell command line the same way, without
running it, for the files it would change:

- redirections into a file (`>`, `>>`, `>|`, `&>`, `>&`, `<>`);
- the files given to a program that changes them: `rm`, `mv`, `cp` (and
  `-t`), `ln`, `install`, `tee`, `chmod`, `touch`, `truncate`, `dd of=`,
  `sed -i`, `perl -i`, `yq -i`, `gawk -i inplace`, `sort -o`, `patch`,
  `rsync`, `tar`, `unzip`, `gzip` and its kin, `curl -o`/`-O`, `wget`,
  `find -delete`/`-exec`, and `git checkout`, `restore`, `rm`, `mv` and
  `config`;
- through `if`, `for`, `while`, `{ }`, `!`, comments, arithmetic and
  wrappers (`sudo -u x`, `timeout 5`, `env`, `nice -n 5`, `busybox`,
  `chronic`, `strace -o log`, `ltrace`, `unbuffer`);
- code run from a string, read again: `eval`, `bash -c`, `su -c`, `$(…)`, a
  here-document or `echo … |` fed to a shell (`ash`, `mksh` and
  `busybox ash` among them, and `sudo -s` or `su`), and `xargs` fed a
  here-string or `echo`;
- after `cd DIR`, `pushd DIR` or `git -C DIR`, in that directory; `cd` and
  `cd $HOME` lead home, as `cd ~` does.

Words are expanded first, as the shell would: `{a,b}`, `~`, `$HOME`,
`${HOME}`, `$PWD`, `${PWD}`, and `*`, `?` and `[…]` against the disk. A
directory is refused when it holds a protected file: `rm -rf tests` is,
`cp x tests/` is not. A git command that changes the whole tree (`stash`,
`reset --hard`, `checkout` or `switch` to a revision, `checkout -p`,
`apply`, `am`, `merge`, `rebase`, `cherry-pick`, `revert`, `clean`) is put
to git, and refused when git says it would change a protected file, or
cannot say (`git pull`, which fetches first; a patch on standard input; an
interactive rebase; a revision git does not know). The `flot` commands that
change whose evidence counts are left to the user: `init --force`,
`trust add`/`remove`/`rotate`, `key init`/`rotate`, `sign`, `import`,
`pull`, `gc` (not `gc --dry-run`), and `run`, `verify` or `diff` with
`--proofs-from` or `-f`. These git and `flot` commands are found as the
files are: past wrappers, in shells, `eval`, `xargs`, `find -exec`, `$(…)`,
PowerShell and `cmd`. A git command with a variable in it is also put to git
as written (`read B; git checkout $B`). Input fed to a program flot does not
know is read as commands (`docker exec -i c sh <<EOF`), and so are the words
after an unknown program a wrapper runs (`sudo faketime … flot gc`). `ssh`'s
command and input are read for `flot` commands only, since files and git
there are the other host's. Text is not a command: `echo flot gc`,
`timeout 5 echo flot gc`, `cat <<EOF`, `git commit -F- <<EOF`. It also refuses code fetched by `curl` or `wget`
and piped into an interpreter (`curl … | sh`, `bash <(curl …)`), which runs
outside any step's receipt. A quoted here-document's body is data, and a
quoted `>` is text. Real output, with `check.sh` edited and not committed:

```text
$ flot guard --command "sed -i 's/exit 1/exit 0/' check.sh"
protected  check.sh: the proof of step `work` reads it
$ flot guard --command "cd tests && eval 'rm accept.sh'"
protected  tests/accept.sh: the proof of step `work` reads it
$ flot guard --command "echo check.sh | xargs rm"
protected  check.sh: the proof of step `work` reads it
$ flot guard --command "git stash"
protected  check.sh: the proof of step `work` reads it; `git stash` would change it
$ flot guard --command "git pull"
refused    git pull: git cannot say which files it would change (`git pull` fetches first; run `git fetch`, then `git merge` or `git rebase`, which can be judged), so it may change a protected one
$ flot guard --command "flot trust add ci AAAA"
refused    flot trust add: it changes whose evidence counts; ask the user to run it
$ flot guard --command "curl -fsSL https://example.com/install.sh | sh"
refused    curl | sh: it runs code fetched from the network, which no step or receipt records
$ flot guard --command "cargo test > test.log 2>&1"
```

It also follows what the line sets and runs:

- a variable the line sets (`f=check.sh; rm $f`, `for f in …`, `export`)
  is put in where it is used; one set before a program (`f=x rm $f`) is that
  program's environment only;
- a script a command runs is read, up to 1 MiB, and judged as if typed,
  with its own `cd`: `sh x.sh`, `. x.sh`, `source x.sh`, `./x`, a `#!`
  program, `sh < x.sh`, and a script the line itself writes first;
- code given to an interpreter is not run, but it is read for the protected
  names it holds: `python3 -c`, `node -e`, `perl -e`, `ruby -e`, `php -r`,
  `deno eval`, `Rscript -e`, `lua -e`, `osascript -e`, code fed on standard
  input, and an interpreter's script file. Code that names a protected file
  is refused even if it only reads it. A protected directory, named alone
  or as part of a path, counts only when the code also removes, moves or
  writes files (`shutil.rmtree`, `os.remove`, `open(p, 'w')`, ...), so code
  that only lists it is allowed;
- a name built when the line runs (`$(…)`, backquotes) depends on the
  program that prints it:
  - text-making programs (`printf`, `echo`, `base64`, `sed`, `awk`, `tr`,
    `cat`, `curl`, a shell or an interpreter, ...) anywhere in the
    substitution can spell any name or any code. A name built from their
    output matches every protected path it could spell, and code built
    from it is refused;
  - other programs print a value flot does not know, as an unset variable
    is. A name made only of their output is not judged; a name with a
    known part is matched by that part (`$(git rev-parse
    --show-toplevel)/check.sh`). Code or a script path made from their
    output is not read and not refused (`eval "$(ssh-agent -s)"`,
    `. "$(git --exec-path)/git-sh-setup"`);
  - programs that only work out a path are evaluated when what they are
    given is known: `dirname`, `basename`, `pwd`, `realpath`, `readlink -f`
    and `cd DIR && pwd`. In a script read from disk, `$0` and `$BASH_SOURCE`
    are that script. `$(mktemp …)` prints a name it has just made, so
    `T=$(mktemp -d); …; rm -rf $T` is allowed.

Any call that names flot's signing key (where `flot key show` says it is,
`FLOT_SIGNING_KEY` included), a retired copy, `host.secret` or their
directory is refused, reads included, with or without a flow: an agent that
reads the key can sign runs as the user. `flot key show` stays allowed.
Real output:

```text
$ flot guard --command "f=check.sh; rm $f"
protected  check.sh: the proof of step `work` reads it
$ flot guard --command "sh clean.sh"
protected  check.sh: the proof of step `work` reads it
$ flot guard --command "python3 -c \"open('check.sh','w').write('exit 0')\""
protected  check.sh: the proof of step `work` reads it; python3 code names it, and flot cannot tell whether that code writes it
$ flot guard --command "sh -c \"\$(printf 'rm che%s' ck.sh)\""
refused    sh $(..): what it runs is built when the command runs, so flot cannot read it before it runs
$ flot guard --command 'eval "$(echo cm0gY2hlY2suc2g= | base64 -d)"'
refused    eval $(..): what it runs is built when the command runs, so flot cannot read it before it runs
$ flot guard --command 'eval "$(ssh-agent -s)"'
$ flot guard --command 'rm -f "$(git rev-parse --show-toplevel)/check.sh"'
protected  check.sh: the proof of step `work` reads it; `$(..)/check.sh` may name it: part of that name is built when the command runs
$ flot guard --command "sh wipe.sh"
protected  tests: it holds tests/accept.sh: the proof of step `work` reads it
$ flot guard --command "python3 -c \"import os; print(os.listdir('tests'))\""
$ flot guard --command "cat ~/.config/flot/signing.key"
secret     ~/.config/flot/signing.key: it is the signing key, or a copy flot keeps of one, and an agent that reads it can sign runs as the user
$ flot guard --command "T=\$(mktemp -d); cargo test > \$T/log 2>&1; rm -rf \$T"
$ flot guard --command "python3 -c 'print(1)'"
```

(`clean.sh` holds `rm -f check.sh`; `wipe.sh`, at the project root, holds
`cd "$(dirname "$0")" && rm -rf tests`, and `tests/accept.sh` is a proof.)
It reads text, so some changes stay out
of its reach, and are what `proofs_from` catches, since a proof that differs
from the revision proves nothing however it was changed:

- a name code assembles from parts (`open('che'+'ck.sh','w')`);
- a variable set from what a program reads or prints
  (`read f < name.txt; rm $f`, `f=$(head -1 name.txt); rm $f`), and names
  another program produces (`find … | xargs rm`, `rm "$(cmd)"`);
- code or a script that a program other than a text-making one prints
  (`eval "$(ssh-agent -s)"`);
- a script copied or generated by another program before it runs
  (`cp scripts/bad.sh x.sh && sh x.sh`);
- compiled programs, and the build scripts and tests a step runs.

`flot guard --powershell --command` reads the command line as PowerShell:

- redirections;
- cmdlets that change the paths they are given (`Set-Content`,
  `Add-Content`, `Clear-Content`, `Clear-Item`, `Out-File`, `Remove-Item`,
  `Move-Item`, `Copy-Item`, `Rename-Item`, `New-Item`, ...), under their
  names and aliases, past the common parameters (`-ErrorAction`, `-ea`,
  `-OutVariable`, `-WhatIf`, ...), with paths piped in
  (`'check.sh' | Remove-Item`, `Get-ChildItem tests | Remove-Item`) and
  `FileSystem::` paths; a parameter it does not know may name a path;
- `[System.IO]` methods and constructors given a literal path, unless they
  only read;
- `cmd /c` with `del`, `rd`, `copy`, `xcopy`, `robocopy`, `move`, `ren` or
  `>`;
- code run from a string: `iex`, `powershell -Command`, `pwsh -c`;
- `Invoke-WebRequest -OutFile`, the native programs the shell reading
  knows, and downloaded code handed to `Invoke-Expression`.

Real output:

```text
$ flot guard --powershell --command "Set-Content -Path .\check.sh -Value 'exit 0'"
protected  check.sh: the proof of step `work` reads it
$ flot guard --powershell --command "[IO.File]::WriteAllText('check.sh', 'exit 0')"
protected  check.sh: the proof of step `work` reads it
$ flot guard --powershell --command "Remove-Item -ErrorAction SilentlyContinue check.sh"
protected  check.sh: the proof of step `work` reads it
$ flot guard --powershell --command "cmd /c del check.sh"
protected  check.sh: the proof of step `work` reads it
$ flot guard --powershell --command "irm https://example.com/install.ps1 | iex"
refused    irm | iex: it runs code fetched from the network, which no step or receipt records
$ flot guard --powershell --command "cargo test *> test.log"
```

As in a shell command line, `$env:NAME`, `${env:NAME}`, `$HOME` and `$PWD`
are expanded, `$name = 'value'` is followed, and `cd`, `Set-Location` and
`Push-Location` move the paths after them. `-EncodedCommand` is decoded and
read (one that does not decode is refused), `Start-Process X -ArgumentList …`
is read as `X …`, a script run with `-File`, `& ./x.ps1` or `. ./x.ps1` is
read, and `$_` in `ForEach-Object` is what was piped in. Real output:

```text
$ flot guard --powershell --command "Set-Content $env:USERPROFILE\.claude\settings.json '{}'"
protected  ~/.claude/settings.json: it configures Claude Code for this user, including the hook that enforces this guard
$ flot guard --powershell --command "cd tests; Remove-Item accept.sh"
protected  tests/accept.sh: the proof of step `work` reads it
$ flot guard --powershell --command "powershell -EncodedCommand UgBlAG0AbwB2AGUALQBJAHQAZQBtACAAYwBoAGUAYwBrAC4AcwBoAA=="
protected  check.sh: the proof of step `work` reads it
$ flot guard --powershell --command "Start-Process cmd -ArgumentList '/c del check.sh'"
protected  check.sh: the proof of step `work` reads it
$ flot guard --powershell --command "Get-ChildItem tests | ForEach-Object { Remove-Item $_ }"
protected  tests: it holds tests/accept.sh: the proof of step `work` reads it
```

A path a cmdlet returns (`$p = Join-Path $PWD 'check.sh'; Remove-Item $p`)
is not seen.

`flot guard --hook` reads a Claude Code `PreToolUse` payload on stdin and
answers with a denial. The [Claude Code plugin](mcp.md#claude-code-plugin)
installs it for every tool (matcher `.*`): it reads an edit's file, a Bash
or PowerShell command line, and, for any other tool not named like a read
(`Read`, `mcp__filesystem__list_directory`, ...), every `path`,
`file_path`, `file`, `filename`, `destination`, `target`, `notebook_path`,
`paths` or `source`, at any depth of its input. Where a flow is, a call the
guard cannot judge (a payload that is not JSON, an edit without its file, a
failure of the guard itself) is denied; where none is, the hook says
nothing. Real output:

```text
$ echo "not json" | flot guard --hook
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "flot: this call cannot be judged (the hook payload is not JSON: expected ident at line 1 column 2), and a flow is here, so it is refused; ask the user to make it."
  }
}
```

In a real headless session (Claude Code 2.1.330, $0.23), Claude was told to
run `sed -i … check.sh` on a proof's file and `rm .github/workflows/ci.yml`.
The hook denied both before they ran, with the reasons above, and Claude
reported that the check still failed instead of working around it.

### What the guard refused

Where the store already exists, the hook also notes each denial in it, and
the next `flot run` records the ones made since the run before it finished
(an agent the run itself started included) in its manifest, which a
signature covers. `flot run`, `flot show` and `flot verify --markdown` list
them, so a reviewer sees what an agent tried, not only what it delivered. A
denial where there is no store yet is not noted, and noting never creates
one. Real output, after the hook refused an `Edit` and a `sed -i` of
`tests/slug.sh`:

```text
$ flot run
ran      docs                 ok        11ms  claims 0v/1u/0r  receipt 74d47914d421
cached   fix                  ok        11ms  claims 1v/0u/0r  receipt c7d08429cb05

UNVERIFIED 1795563098731-60af8505578c  root 60af8505578c  75ms
limitations:
  - docs: the proof passes with the changes undone to main (e023c8566969) too, so it shows nothing this change did; claims stay unverified
  - proofs checked against main (e023c8566969)
refused by the guard since the previous run:
  - Edit: tests/slug.sh is protected: the proof of step `fix` reads it
  - Bash: tests/slug.sh is protected: the proof of step `fix` reads it
```

`flot guard --replay [PATHS]` reads Claude Code transcripts (`.jsonl` files,
or directories holding them; by default `~/.claude/projects`) and judges
every call in them the hook would see (edits, shell commands, and other
tools that name a path) as it would now, against the flows as they are now,
listing what it would refuse. It notes nothing. A call whose directory is gone is counted, not
judged. Over this machine's transcripts (real output):

```text
$ flot guard --replay
refused  2026-11-24T14:43:39.614Z  Write  /tmp/tmp.NYNgCBazmQ: check.sh is protected: the proof of step `work` reads it
117 transcripts, 4725 edits and commands: 33 where a flow is, 1 refused, 85 not judged because their directory is gone
```

The one refused is a session that told Claude to tamper with a proof; two
calls of another such session were refused too while its directory
(`/tmp/hooktest`) existed, and are now among those not judged. The 32 other
calls where a flow is were let through. That is a sample of one developer's
sessions, not a false-positive rate. With flot 0.2.5, over the same
developer's Windows transcripts, 755 transcripts and 6960 calls (231 of
them `PowerShell`), 14 where a flow is, none was refused. Of the 231
PowerShell command lines, 9 named files they write (a `Set-Content` of a
doc, `New-Item` junctions, a `Move-Item` and `Rename-Item` of a credentials
file); one more name, `main` from `git checkout main`, was a branch read as
a path. That command is now read as changing the whole tree, and put to
git.

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
ran      fix                  failed    11ms  claims 0v/1u/0r  receipt a3e38ae62af5

FAILED 1795655385832-8f0f0b8fcf25  root 8f0f0b8fcf25  39ms
limitations:
  - fix: changed files outside may_change: check.sh
  - fix: claims are unverified because the step did not succeed
  - proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)
```

Every file the scan reaches is hashed before and after each step outside a
candidate's copy, for this check and for the files the step answers for (see
[Files a step changed](#files-a-step-changed)), so each execution costs two
reads of the project. A step with `may_change` runs alone: no other step of
the run runs beside it, so what changed is its own doing (its proof, and
candidates in their own copies, are not held back). The scope is part of the
step key, so narrowing it runs the step again. A change of mode alone is not
a change of content; it is neither reported nor recorded.

## Sandbox

`may_change` judges what a step changed; `sandbox` stops it from changing
anything else, and from reaching the network. The kernel enforces it, on Linux
and macOS:

```toml
[[step]]
id = "fix"
run = "sh agent.sh"
inputs = ["agent.sh"]
sandbox = { write = ["src"] }
claims = ["src/slug.txt is fixed"]
verify = "grep -q fixed src/slug.txt"
```

- `network` (default `false`): without it, on Linux the step runs in a new
  user and network namespace whose only interface is an unconfigured
  loopback; on macOS a Seatbelt profile denies it every network operation,
  `127.0.0.1` and Unix sockets included.
- `write`: paths, relative to the flow root, the step may create, edit,
  delete or rename files beneath. Absent, writes are not restricted. Present,
  a Landlock ruleset (Linux) or the Seatbelt profile (macOS) denies every
  other write: everywhere else, including `/tmp`, `.git` and the store.
  `/dev/null`, `/dev/zero`, `/dev/full` and `/dev/tty` (and on macOS
  `/dev/fd`) stay writable, and the step gets a scratch directory of its own
  as `$TMPDIR`, where its `$FLOT_METRICS` file also is; it is removed after the
  step. Each path must exist.

On macOS the step's shell is started by `/usr/bin/sandbox-exec` with the
profile, which every process the step starts inherits. Apple marks
`sandbox-exec` deprecated, but ships it.

Reads are not restricted, so a confined step can still read your files, your
credentials and the environment flot passes it; this is not a container. Nor
are other processes: a confined step can still ask a service that runs
outside it (over D-Bus on Linux, over Mach or XPC on macOS) to write for it.
`network = true` with no `write` restricts nothing, and is refused.

Here `agent.sh` fixes `src/slug.txt` and then tries to rewrite `check.sh`
(real output):

```text
$ flot run
ran      fix                  failed    12ms  claims 0v/1u/0r  receipt c6cdaf4de3e8
    ! agent.sh: 2: cannot create check.sh: Permission denied

FAILED 1795655385932-a06efe1cad1c  root a06efe1cad1c  40ms
limitations:
  - fix: claims are unverified because the step did not succeed
  - proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)
```

and the same step fetching a page instead (its first two lines):

```text
ran      fix                  failed    12ms  claims 0v/1u/0r  receipt e19b772cf460
    ! curl: (6) Could not resolve host: example.com
```

The receipt records the policy and how it was enforced, so a reader can see
the step was confined:

```text
$ flot receipt c6cdaf4de3e8 | jq -c .identity.sandbox
{"network":false,"write":["src"],"mechanism":"landlock abi 7, network namespace"}
```

On macOS the mechanism reads `seatbelt file writes, seatbelt network`.

A step that asks for a sandbox runs in it or not at all. Where it cannot be
enforced (neither Linux nor macOS, a kernel without Landlock, a `write`
policy on Landlock below ABI 3 (Linux 6.2), which cannot deny truncating a
file, a host that
does not let an unprivileged process have a user namespace, a macOS without
`sandbox-exec`, a `write` path that does not exist) the step is not run: it fails with "could not run: its sandbox cannot
be enforced here" and the reason, such as "cannot deny truncation on
Landlock ABI 2; ABI 3 (Linux 6.2) is required". `flot doctor` fails its
`sandbox` check there too. The policy is part of the step key. The
proof runs unconfined, like any proof, and so does the rest of the flow.

Tested where both are enforced: WSL2 with kernel 6.18 (Landlock ABI 7),
GitHub's `ubuntu-latest` runners, which refuse `unshare -Urn` (it maps root)
but let flot create the namespaces it needs, and GitHub's `macos-latest`
runners. The same test checks that a confined step writes and hard-links
beneath `src` but cannot write a file outside it or hard-link one into it,
and that a step without network cannot connect to a port on `127.0.0.1`
that an unconfined step reaches. With the network rule taken out of the
macOS profile, that test fails there. On Windows the step is not run.

Landlock needs stable inodes: on file systems without them, such as WSL's
`/mnt` drives (9p), writes beneath an allowed path are denied too, so a
confined step fails there rather than escaping.

## Outputs

`outputs` names the files a step produces. When the step finishes
their content goes into the store and the receipt, and a success that produced
no file for a declared pattern is `failed`: it did not do what the flow says.
Reusing a receipt puts missing outputs back, so the files on disk are the ones
its claims were proven against. An output that exists with different content
is never overwritten, and never vouched for by a receipt that proved other
content: the receipt is not reused and the step runs again, so its proof judges
the file that is actually there. On reuse the patterns are matched again too:
a file that matches one now but is not in the receipt was never judged by its
proof, so the step runs again
(`gen: out/bad.txt matches an output pattern but is not in its receipt; the step runs again`).
Output files travel in a bundle, so an importing host gets the produced
artifact, not just a report about it. Two steps that
produce the same file without one needing the other are reported, because which
content survives depends on scheduling. Patterns must stay inside the flow root,
a step cannot declare its own proof as an output, and a receipt is never reused
if it names a file the local flow does not declare for that step. A file reached
through a symbolic link is neither recorded as an output nor restored through:
the link could point anywhere. Outputs are what the patterns match
when the step finishes, so a stale file left by an earlier run is recorded too;
keep patterns as specific as the step's real products.
