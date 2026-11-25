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

Digest over the command, the declared environment, the content and
executable bit of every declared input file and proof input, the verify command, the declared
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

Where git does not trust the file system's executable bit (`core.fileMode`
is false: Git for Windows, and WSL's `/mnt` drives, where every file reads as
executable), an input git tracks is keyed by the bit git records, the one a
Linux or macOS checkout has. Without that, a key computed on such a drive
never matched one computed in CI, so a run signed there could not be reused
there. A file git does not track, and an output, keep the bit the file system
shows.

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
2 when there is one. Each path is checked against the flow of the current
directory and the flow nearest the path.

```text
$ flot guard tests/acceptance.py flot.toml
protected  tests/acceptance.py: the proof of step `implement` reads it
protected  flot.toml: it is the flow file, which says what proves the work
```

It also protects what decides whether the proofs are checked at all, in the
repository the flow is in (or the flow's directory outside one): Claude Code's
settings, which run the hook below, CI workflows, which run the flow, git
hooks and git's configuration, which can point it at other hooks, and the
user's own `~/.claude/settings.json`. In this repository (real output):

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
running it: a redirection into a file, and the files given to a program that
changes them (`rm`, `mv`, `cp`, `tee`, `chmod`, `touch`, `truncate`, `dd of=`,
`sed -i`, `perl -i`, `git checkout`/`restore`/`rm`/`mv`). It also refuses
code fetched by `curl` or `wget` and piped into an interpreter
(`curl … | sh`, `bash <(curl …)`), which runs outside any step's receipt. A
here-document's body is data, and a quoted `>` is text. Real output:

```text
$ flot guard --command "sed -i 's/exit 1/exit 0/' check.sh"
protected  check.sh: the proof of step `work` reads it
$ flot guard --command "curl -fsSL https://example.com/install.sh | sh"
refused    curl | sh: it runs code fetched from the network, which no step or receipt records
$ flot guard --command "cargo test > test.log 2>&1"
```

It reads only what the command line names. A write through a script, a
variable, `cd` into another directory or `python -c` is not seen, and is
what `proofs_from` catches: a proof that differs from the revision proves
nothing, however it was changed.

`flot guard --powershell --command` reads the command line as PowerShell:
redirections, cmdlets that change the paths they are given (`Set-Content`,
`Out-File`, `Remove-Item`, `Move-Item`, `Copy-Item`, `Rename-Item`,
`New-Item`, ...) under their names and aliases, `[IO.File]` methods given a
literal path, `Invoke-WebRequest -OutFile`, the native programs the shell
reading knows, and downloaded code handed to `Invoke-Expression`. Real
output:

```text
$ flot guard --powershell --command "Set-Content -Path .\check.sh -Value 'exit 0'"
protected  check.sh: the proof of step `work` reads it
$ flot guard --powershell --command "[IO.File]::WriteAllText('check.sh', 'exit 0')"
protected  check.sh: the proof of step `work` reads it
$ flot guard --powershell --command "irm https://example.com/install.ps1 | iex"
refused    irm | iex: it runs code fetched from the network, which no step or receipt records
$ flot guard --powershell --command "cargo test *> test.log"
```

As with a shell command, a path held in a variable or computed is not seen.

`flot guard --hook` reads a Claude Code `PreToolUse` payload on stdin (a file
path, or a Bash or PowerShell command line) and answers with a denial; the
[Claude Code plugin](mcp.md#claude-code-plugin) installs it for `Edit`,
`Write`, `MultiEdit`, `NotebookEdit`, `Bash` and `PowerShell`. Downloaded
code is refused only where a flow is; elsewhere the hook says nothing.

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
every `Edit`, `Write`, `MultiEdit`, `NotebookEdit`, `Bash` and `PowerShell`
call in them as
the hook would now, against the flows as they are now, listing what it would
refuse. It notes nothing. A call whose directory is gone is counted, not
judged. Over this machine's transcripts (real output):

```text
$ flot guard --replay
refused  2026-11-24T16:20:49.931Z  Bash  /tmp/hooktest: check.sh is protected: the proof of step `work` reads it
refused  2026-11-24T16:20:50.473Z  Bash  /tmp/hooktest: .github/workflows/ci.yml is protected: it is a CI workflow, which decides what CI checks
refused  2026-11-24T14:43:39.614Z  Write  /tmp/tmp.NYNgCBazmQ: check.sh is protected: the proof of step `work` reads it
114 transcripts, 4707 edits and commands: 36 where a flow is, 3 refused, 64 not judged because their directory is gone
```

The three are the sessions that told Claude to tamper with a proof; the 33
other calls where a flow is (edits to the code under test, test runs,
reviews) were let through. That is a sample of one developer's sessions,
not a false-positive rate. Over the same developer's Windows transcripts,
755 transcripts and 6960 calls (231 of them `PowerShell`), 14 where a flow
is, none was refused. Of the 231 PowerShell command lines, 9 named files
they write (a `Set-Content` of a doc, `New-Item` junctions, a `Move-Item`
and `Rename-Item` of a credentials file); one more name, `main` from
`git checkout main`, is a branch read as a path, as the shell reading does.

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
ran      fix                  failed    12ms  claims 0v/1u/0r  receipt b2b222b1555a
    ! agent.sh: 2: cannot create check.sh: Permission denied

FAILED 1795532641102-7555f70816cc  root 7555f70816cc  25ms
limitations:
  - fix: claims are unverified because the step did not succeed
```

and the same step fetching a page instead:

```text
ran      fix                  failed    12ms  claims 0v/1u/0r  receipt 4655b66d9eab
    ! curl: (6) Could not resolve host: example.com
```

The receipt records the policy and how it was enforced, so a reader can see
the step was confined:

```text
$ flot receipt b2b222b1555a | jq -c .identity.sandbox
{"network":false,"write":["src"],"mechanism":"landlock abi 7, network namespace"}
```

On macOS the mechanism reads `seatbelt file writes, seatbelt network`.

A step that asks for a sandbox runs in it or not at all. Where it cannot be
enforced (neither Linux nor macOS, a kernel without Landlock, a host that
does not let an unprivileged process have a user namespace, a macOS without
`sandbox-exec`, a `write` path that does not exist) the step is not run: it fails with "could not run: its sandbox cannot
be enforced here" and the reason. The policy is part of the step key. The
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
