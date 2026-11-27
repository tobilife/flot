# CLI reference

| Command | Purpose |
|---|---|
| `flot init` | write a starter `flot.toml` in the current directory; in a clone whose `origin` names a default branch it sets `proofs_from` to it |
| `flot plan [--only STEP] [--force] [--proofs-from REV]` | predicted execution, reuse and evidence gaps; runs nothing; reuse that rests on an [`always`](flows.md#steps-that-always-run) step coming out as last time is marked `?` |
| `flot run [--only STEP] [--force] [--jobs N] [--max-cost USD] [--sign] [--proofs-from REV] [--progress WHEN]` | execute and record receipts; `--only` runs a step and what it needs, a [partial](flows.md#verdict) run; `--sign` then signs the run; `--proofs-from` holds each proof to a git revision ([proofs from a revision](flows.md#proofs-from-a-revision)); `--progress` (`auto`, `live`, `plain`, `off`) chooses how the steps are shown on stderr as they run ([progress](#progress)) |
| `flot sign [RUN]` | sign a run executed here with the host key |
| `flot show [RUN]` | steps, claims and limitations of a run |
| `flot verify [RUN] [--proofs-from REV] [--markdown]` | re-authenticate a run from the store alone; with a revision (or the flow's `proofs_from`), also check its proofs are as they are there; `--markdown` prints the check as a report ([in CI](ci.md)) |
| `flot doctor` | check what the evidence relies on here: the flow parses, `proofs_from` is set and resolves, every proof declares the files it reads, git ignores the store, the signing key and trust list, no flaky step or proof in recent runs, that a requested sandbox can be enforced (on Linux, a `write` policy needs Landlock ABI 3), and that no Claude Code settings switch hooks off; exits 1 if a check fails. It changes nothing, except that where a store exists it creates this host's secret (`host.secret`, beside the signing key) if there is none |
| `flot guard [PATH...] [--command CMD [--powershell]] [--hook] [--replay]` | name the paths whose change would touch the proofs or decide whether they are checked (CI workflows, git hooks, Claude Code settings and plugins, the flot binary), or a directory holding one, and exit 2 if any; `--command` reads a shell command line (a PowerShell one with `--powershell`) for the files it writes, the whole-tree git commands git says would change one, downloaded code piped into an interpreter and the `flot` commands that change whose evidence counts; `--hook` answers a Claude Code `PreToolUse` payload for any tool, denies a call it cannot judge where a flow is, and notes a denial in the store for the next run to record; `--replay` judges every call in Claude Code transcripts (the PATHS, by default `~/.claude/projects`) as the hook would now and lists what it would refuse ([refusing the edit](flows.md#refusing-the-edit)) |
| `flot diff [LEFT] [RIGHT]` | step-by-step comparison by receipt identity; by default `latest~1` against `latest`; under each changed step, why, read from its two receipts ([diff](#diff)) |
| `flot receipt ID` | one receipt as JSON (id prefix accepted) |
| `flot log STEP [RUN] [--stderr] [--candidate NAME] [--attempt N]` | complete output one step of a run printed; by default the receipt the run kept, else another candidate it ran or an earlier attempt |
| `flot cat DIGEST` | one stored object, byte-exact |
| `flot runs [--limit N]` | recorded runs, newest first; imported ones are marked |
| `flot search QUERY [--runs N] [--limit N]` | lines of recorded output, claims and limitations that contain QUERY (literal, any case), newest run first; a reused step is searched in the run that executed it, and attempts and candidates not kept are searched too |
| `flot flaky [--runs N]` | name the steps and proofs that gave different results on the same inputs in runs executed here; exits 2 if any |
| `flot gc [--dry-run]` | drop evidence no recorded run references |
| `flot bundle [RUN] [-o FILE] [--sign] [--allow-secrets]` | one portable file: run, receipts, every object they name (streams, produced files, proofs), signatures; a run that carries a credential-shaped token is refused |
| `flot import FILE [--require-trusted]` | authenticate a bundle and absorb it into this store |
| `flot push [RUN] [--remote R] [--sign] [--allow-secrets]` | put a run's bundle on a git remote (default `origin`) in the ref `refs/flot/runs`, beside the runs already there; refused like `bundle` |
| `flot pull [--remote R] [--require-trusted]` | import this flow's runs from a git remote's `refs/flot/runs` that this store does not hold |
| `flot key init` / `flot key show` | create or print the host signing key |
| `flot key rotate [-o FILE]` | replace the host key, keep the old secret beside it, write a handover both keys sign |
| `flot trust add NAME KEY` / `remove` / `list` | decide which signers this host believes for this store |
| `flot trust rotate HANDOVER [--revoke]` | follow a trusted signer to the key their handover names |
| `flot mcp` | serve the same contract over MCP on stdio |

## Selecting a run

`RUN` is `latest` (the newest run executed here; an imported run is never
`latest`), `latest~N` (the run executed here N runs before it, so `latest~1` is
the previous one), a full run id, or a prefix of at least four characters of a
run id or of the root digest it ends with.

A run id starts with the millisecond the run started, and runs are counted
back in that order. When this host's clock reads no later than the start of
the newest run executed here, as after the clock was set back, the run's start
is recorded 1 ms after that one's and the run lists the limitation `this
host's clock read no later than the start of the newest run recorded here`, so
`latest~1` is still the run before. Imported runs keep their own host's clock
and move nothing.

`flot run` prints one line per step. A step that executed also shows the first
and last lines of its output (the middle is marked as omitted when it exceeds
`max_inline_bytes`) and, when something was left out, the `flot log` command
that prints all of it. A reused step prints only its line; its output is still
one `flot log` away. A step reused from an imported signed run ends its line
with `vouched for by NAME`: the name this store trusts the signer under, when
the store holds a run that signer signed that kept the same receipt for the
step. Otherwise the signer's key is followed by `(not trusted here)`, once
the trust list no longer holds it, or `(not confirmed here)`, when no such
run is here. `flot show` and `flot verify` name signers the same way.
`flot show` points each failed step at its `flot log`.

## Progress

While it runs, `flot run` shows its steps on stderr: each as it starts, the
lines it prints, and how it ended. What it prints on stdout, its `--json`
and its exit code are the same whatever `--progress` asks for, and so is
every byte the run records.

- `live` keeps a row for each step running, with its time so far, below
  what the steps print. Each line is tagged with its step: `[build]` for
  its stdout, `[build:stderr]`, and `[build:proof]` for what its proof
  prints. A step that ends leaves one line: `✓` when it passed and `✗`
  otherwise, then what it came to (`passed`; `unverified` or `refuted`
  when it succeeded but a claim is unproven or disproven; `failed`,
  `timeout` or `cancelled`), its time, and how many of its claims held. The
  time is the whole step: its command, its proof and flot's own checks
  around them, so it is longer than the command's time the summary on
  stdout gives; under 0.1 s it is in milliseconds. A step that was not run
  says why instead: `✗ ship not run: dependency lint (failed) did not hold`. A
  reused step's line starts with `cached`. The rows are taken back with
  cursor-up and clear-line alone (the cursor is never hidden and the screen
  never switched), so a run stopped at any point leaves the terminal as it
  was, and they are gone before the summary is printed. Colour unless `NO_COLOR` is set. On Windows the
  console must take escape sequences; one that does not gets `plain`.
- `plain` prints one line per event, with no escape sequences, for logs.
- `off` prints nothing.
- `auto`, the default, is `live` when stderr is a terminal and `TERM` is
  not `dumb`, and `plain` otherwise, as in CI.

A line a step prints is shown once it ends, or once its stream does, and
whole: lines of steps that run at once (`--jobs`) never mix. What a step
prints past its `max_output_bytes` is not shown, as it is not kept. Over MCP
nothing is shown while a run goes.

For a flow whose `build` and `test` print as they go, each proving its
claim, and whose `ship` needs both, `flot run --progress plain` printed on
stderr:

```
run 3 steps, 1 job
start build
[build] compiling part 1
[build] compiling part 2
[build] compiling part 3
[build] compiling part 4
[build:stderr] warning: one thing
[build:proof] proof ok
done build passed 1.6s claims 1/1
start test
[test] test 1 ok
[test] test 2 ok
[test] test 3 ok
done test passed 1.5s claims 1/1
start ship
[ship] shipping
done ship passed 16ms
run finished 3.2s
```

Run again with nothing changed, every step is reused:

```
run 3 steps, 1 job
cached build passed claims 1/1
cached test passed claims 1/1
cached ship passed
run finished 28ms
```

A step that may take more attempts says which one each line is
(`start flaky attempt 2/2`), a step with candidates which candidate
(`start pick candidate fast`), and a step that was not run says why, with no
time: `done ship not run: dependency lint (failed) did not hold`.

## Diff

`flot diff` names each step whose receipt changed, and under it why, read
from the two receipts. A different step key means the work was not the same:
`command changed`, `inputs:` or `proof inputs:` with the files that changed,
were added or were removed (five named, the rest counted), `dependencies:`
with the steps whose receipts it built on changed, `derived by flot A -> B`,
`claims or their proof command changed`, or, when none of those, the rest of
the step's definition (its env, outputs or settings). The same key with a
different receipt is `same key, different result:` and what differs, in this
order: the exit code, claim results, outputs, other files it changed, stdout,
stderr (last, since output that prints a time differs on every run). A
receipt that is gone from the store, or whose body no longer matches its id,
tells nothing, and the step says so. In `--json` each step has `reasons`.
A run that selected only some steps shows `passed, partial: 1 of 2 steps`
(`left_partial`/`right_partial` in `--json`), and a step it left out is `not
selected in left` (or `right`), not `added` or `removed`, while both runs
are of the same flow.

Real output, comparing this repository's self-check before and after one
file changed (exit 2, as for any difference):

```text
$ flot diff 1795738845615-033e13c9ff66 1795738950533-a7b9115bcf58
left  1795738845615-033e13c9ff66  passed
right 1795738950533-a7b9115bcf58  passed
  build                receipt changed          21c60ee3af20 -> 3602298f68a4
        inputs: src/core/receipt/check.rs changed
        dependencies: format
  format               receipt changed          df655231df15 -> 7ffde95c0a58
        inputs: src/core/receipt/check.rs changed
  lint                 receipt changed          d20bcda1c198 -> 0b2171b1ee65
        inputs: src/core/receipt/check.rs changed
        dependencies: build
  test                 receipt changed          f64bf779de3b -> 69dd690f164c
        inputs: src/core/receipt/check.rs changed
        dependencies: build
```

## Selecting a flow

The flow is the nearest `flot.toml` in the current directory or above it, or
exactly the file `-f` names (relative to the current directory). Paths in the
flow and the `.flot` store are relative to the flow file's directory, so one
flow always has one store, however it is named.

## Machine-readable output

`--json` gives machine-readable output for `plan`, `run`, `show`, `verify`,
`diff`, `runs`, `gc`, `bundle`, `import`, `key` and `trust`; `flot receipt`
always prints JSON. Human output shows control characters and bidirectional
overrides as visible escapes, since step output and claims can come from an
imported bundle. JSON output escapes the same characters as `\uXXXX`, which a
JSON reader decodes to the same text; `flot cat` is byte-exact. `run` and
`verify` carry `result`, the judgment their exit code gives
([verdict](flows.md#verdict)).

## Exit codes

`0` passed; `1` failed, a `flot doctor` check that fails, or evidence that does not authenticate (a
broken run, a tampered receipt or object, a bundle that fails its checks, a
store record that cannot be read); `2` unverified claims, a run of some of
the flow's steps that passed (`--only`, a [partial](flows.md#verdict) run), a flaky step or proof, a non-identical
diff, a run whose proofs differ from the revision `verify` holds it to, or a
path `flot guard` protects; `3` invalid request, including usage errors and policy refusals such as
`--require-trusted`; `4` the environment failed (IO, permissions, a store busy
with a run), so retrying can succeed.
