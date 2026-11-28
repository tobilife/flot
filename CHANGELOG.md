# Changelog

## Unreleased

- `flot explain [RUN] [STEP]` and the MCP tool `flot_explain` say why a run
  did not pass, at a glance: what keeps the run as a whole from passing
  (evidence that does not authenticate, only some steps run, only the
  proofs differ), and for each step that did not hold, each cause read from
  its receipt (its command failed or timed out, it did not run, its sandbox
  cannot be enforced here, its proof changed since `proofs_from`, no proof,
  refuted, failing with its `held` files, passing before the change, or
  mutants that survived), with the command that shows more: `flot log STEP
  --mutants`, `flot verify`, `flot doctor`.

## 0.2.13

A pull request that changes a proof says so: `flot verify` and the GitHub
action name the case where only the change to the proofs keeps a run from
passing, and the job stays red for a person to review. Found by using flot
on itself.

- `flot verify` says `only the proofs differ` (`only_proofs_differ` in
  `--json`) when the change to the proofs is all that keeps a run from
  passing: no step failed, and every claim left unverified is one of a step
  whose proof differs from the revision. A pull request that changes a
  proof still fails the action's job, which now names this case in its
  annotation and its report's headline, `flot: only the proofs differ from
  …`, so it reads apart from work that fails; the action also outputs
  `only-proofs-differ`. A claim left unverified because its proof differed
  records the commit and how, so `flot verify` names that step's change
  even where the receipt does not keep it, such as `held` files added to a
  step.

## 0.2.12

A proof can be held to the mutants a mutation tester makes of the work,
proofs on copies of the work keep their copy and rebuild only what
changed, and flot locks its store with the standard library. Found by
using flot on itself and on open-design.

- A step can hold its proof to the mutants a mutation tester makes of the
  work: with `mutants = { run, report, allow }`, once the proof passes the
  tester runs with `$FLOT_MUTANTS_DIR` for its report and
  `$FLOT_PROOFS_FROM` for the revision, and the claims verify only if every
  mutant is caught or allowed by an entry of the allow list, which names the
  file, the mutation and a digest of the code it changes, with a reason.
  The allow list is a proof input, so only one the revision has counts.
  Reports in the mutation testing report schema (StrykerJS) and
  cargo-mutants' `outcomes.json` are read; one with a mutant left untested
  verifies nothing. `flot show` names each survivor as an entry would, and
  `flot log STEP --mutants` prints what the tester printed. On open-design,
  StrykerJS on the lines a change added found the two mutants a loosened
  test let through; on flot, cargo-mutants on this change made 145 mutants
  and found the one an assertion removed let through.
- Proofs on copies of the work, for `fails_before` or `held`, run in one
  copy kept in `.flot/builds/work` and build Rust in `.flot/builds/target`,
  held by one proof at a time, instead of a new copy each time and a build
  directory per step. Only the files that changed since the last proof are
  written again, stamped past its build, so `cargo` rebuilds only them;
  anything else a proof left in the copy is removed. The copy builds without
  debug info or incremental compilation unless the step's environment asks
  for them. On flot itself, with two steps held and a test edited each run
  (medians of four, versions run in turn): the builds take 262 MB instead
  of 1,621 MB, a run went from 69 to 58 s with one build directory, and
  from 56 to 46 s with the kept copy. `flot run` removes the copy and its
  build once no proof has used them for seven days, and the per-step
  directories earlier versions made.
- A copy of the work is written when it is made. On Windows, and on macOS,
  where a copy is a clone, a copied file kept its modification time, so
  `cargo` in a copy could take a file older than its last build there for
  the version it built, and prove what an earlier copy held: the revision's
  files a `fails_before` check put back, or the held files.
- Building flot needs Rust 1.89 (it was 1.88): the store and the copies'
  build are locked with the standard library's file locks instead of calls
  to the system through `unsafe` code.
- `flot guard` names flot's own binary as the binary that enforces the
  guard wherever it is. A binary built by a proof on a copy of the work sits
  in the store, and was named as evidence in the store instead.

## 0.2.11

A step can hold its proof to the tests as the revision has them, a proof
on a copy of the work no longer hands the work's `cargo` the copy's
binaries, and reading files is faster. Found by using flot on itself.

- A step with `held = ["tests/**"]` verifies only if its proof also passes
  on a copy of the work where those files are as the `proofs_from` revision
  has them, so a change that passes only tests it loosened, deleted or
  replaced proves nothing, while the tests stay free to edit. `flot log STEP
  --held` and MCP `flot_step_output` with `held: true` print what that run
  printed, and `flot show` says how it ended.
- A proof run on a copy of the work, for `fails_before` or `held`, builds
  Rust in `.flot/builds/STEP` instead of in a `CARGO_TARGET_DIR` it shares
  with the work. Before, the work's next `cargo` run could take the copy's
  binaries, built from other sources at a path that no longer exists, as
  its own. `flot gc` removes these directories.
- `flot log STEP --proof` prints what the step's proof printed, stdout and
  stderr together, and MCP `flot_step_output` takes `proof: true`. Before,
  a refuted claim's evidence was reachable only through its receipt's digest
  and `flot cat`. `flot run` and `flot show` point a step with a refuted
  claim at it, and the plugin's skill tells Claude to read it.
- Reading a project's files is faster, most on a network or 9p mount (a
  repository on a Windows drive under WSL2). Files are read several at a
  time and each with one round trip fewer, patterns starting in the same
  directory share one walk of it, git is asked for the executable bits it
  records only when a scan found files, and within one run a file is read
  again only when the file system says it changed since it was read: its
  size, inode, mode, modification time or change time differ, so a rewrite
  that keeps the size and puts the modification time back is still seen. A
  file changed after the run began is read every time; that reuse is on
  Linux and macOS only, and `flot plan` reads every file. Keys and recorded
  changes are the same as before. Release builds of 0.2.10 and 0.2.11 run
  in turn, medians: on a 2,320-file repository on `/mnt/d`, a run that
  reuses every step went from 21.2 s to 3.5 s and `flot plan` from 9.2 s to
  2.0 s; on the same repository on ext4, from 256 to 82 ms and from 107 to
  39 ms; this repository's own check from 2.1 s to 0.68 s.
- `flot log` without a step names the steps of the latest run instead of
  only saying a step is required.
- `flot doctor` names every proof that declares no `verify_inputs` on one
  `proof inputs` line instead of one line per step.
- The plugin's skill tells Claude that a step still `cached` after it
  changed a file the step depends on does not name that file in its
  `inputs`, and that the reuse proves nothing about the change.

## 0.2.10

`flot plan` predicts the reuse that follows an `always` probe, `flot diff`
says why each step changed, and a clock set back can no longer reorder
runs. Found by using flot on itself.

- `flot plan` no longer shows every step after an `always` probe as running.
  It predicts on what the probe recorded last time, marks each reuse that
  prediction makes with `? reused if toolchain comes out as last time`, and
  lists the probes it rests on in `assumes` in `--json` and over MCP. Before, a plan for this repository with nothing
  changed said all five steps would run; the run then reused four.
- `flot diff` says why each changed step changed, read from its two receipts:
  the files it or its proof read (`inputs: src/a.rs changed, b.rs added`),
  the dependencies whose receipts changed, its command, its claims, the flot
  that derived it, or, with the same key, what came out otherwise (`same key,
  different result: exit 0 -> 1, stderr`). In `--json` and over MCP, as
  `reasons`. A receipt that is gone or altered tells nothing, and the step
  says so.
- A run never sorts behind the one before it. A run id starts with the
  start time, and `latest~N`, `flot runs`, `flot flaky` and `flot diff`
  count back in that order, but `latest` names the run recorded last; when
  this host's clock was set back between two runs, the two disagreed. A test
  failed this way once in this repository's self-check on WSL2: its three
  runs' ids put the second before the first, by about 0.7 s. Such a run's start is now recorded 1 ms after
  the newest run's, with a limitation saying so.
- `flot diff` against a partial run said the steps it left out were `added`
  (or `removed`) and showed its verdict as a plain `passed`. It now says `not
  selected in left`, shows `passed, partial: 1 of 2 steps`, and gives
  `left_partial`/`right_partial` in `--json`.
- The progress view says why a step was not run (`✗ ship not run: dependency
  lint (failed) did not hold`) instead of `cancelled`, which read as if the
  run had been stopped. Times under 0.1 s are in milliseconds (`16ms`, not
  `0.0s`). docs/cli.md says what the time covers: the whole step, proof and
  flot's checks included, so more than the command's time in the summary.
- The plugin's skill tells Claude that a run passing after a failure with
  nothing changed is a flaky step, not a fix (`flot_diff` says `same key,
  different result`), and that a step not run is reported by the dependency
  that failed. A test fails if the skill names an MCP tool flot does not have.

## 0.2.9

The guard finds git and `flot` commands with the same reading that finds the
files a command line changes; its second, cruder lexer is gone. Measured
against 0.2.8 on the corpus, 656 real scripts and this machine's
transcripts: no real script or recorded call changes its verdict.

- Commands run through `chronic`, `strace`, `ltrace` (with their value
  options, so `strace -o log rm check.sh` is `rm`) and `unbuffer`, and by the
  shells `ash`, `mksh` and `busybox ash`, are judged. Before, `chronic rm
  check.sh` and `ash -c 'flot gc'` got through.
- `sudo -s`/`sudo -i` with no command, and `su` with no `-c`, read their
  input as a shell, so `sudo -s <<EOF … EOF` is judged. Input fed to a
  program flot does not know, and an unknown program a wrapper runs, are
  read for `flot` and git commands.
- `ssh`'s command and input are read for `flot` commands (the host may be
  this one, or share the store). A git command there changes the other
  host's repository and is not put to local git.
- A git command with a variable in it is also put to git as written, so
  `read`, `eval`, `printf -v`, a sourced script or PowerShell's
  `Set-Variable` cannot slip a revision past the guard.
- In a script read from disk, `X0` inside double quotes is no longer taken
  for `$0` (`"v1.0"` was read as `v1<script>`).
- `cd` and `cd $HOME` lead home for the git commands after them, as `cd ~`
  does.
- Text is no longer a `flot` command: `timeout 5 echo flot gc`, `cat <<EOF
  … flot gc … EOF`, `git commit -F- <<EOF …` and `iex 'flot gc'` in a POSIX
  shell are allowed.

`flot run` shows its steps on stderr as they run: each as it starts, every
line it and its proof print, tagged with the step, and how it ended, with
its time and how many of its claims held. On a terminal a row per running
step, with its time so far, stays below that output and is redrawn with
cursor-up and clear-line alone; elsewhere, as in CI, each event is one plain
line. `--progress auto|live|plain|off` chooses (`auto`: live on a terminal
whose `TERM` is not `dumb`). Lines of steps run at once never mix. Stdout,
`--json`, exit codes and every recorded byte are unchanged; the receipt
goldens and the runner's scenario golden pass as they were, and a run told
to a sink leaves the same bytes as one that is not. Over MCP nothing is
shown while a run goes.

## 0.2.8

The guard's remaining known holes, measured with the probes that found them
(each got through 0.2.7 with exit 0):

- A variable the line sets is followed: `f=check.sh; rm $f` is refused.
- Scripts a command runs are read and judged (`sh clean.sh`, `. x.sh`,
  `./x`, a `#!` program, PowerShell `-File`), up to 1 MiB each.
- Interpreter code is read for the protected names it holds: `python3 -c
  "open('check.sh','w')…"` is refused, `python3 -c 'print(1)'` is not.
- A name built when the line runs matches every protected path it could
  spell, and code built for `sh` or `eval` is refused
  (`sh -c "$(printf 'rm che%s' ck.sh)"`); `$(mktemp …)` stands for a fresh
  name, so `T=$(mktemp -d); …; rm -rf $T` stays allowed.
- PowerShell: `$env:USERPROFILE` and the other environment variables,
  `$HOME`, `$PWD`, `$name = '…'`, `cd`/`Set-Location`, `-EncodedCommand`,
  `Start-Process` and `$_` in `ForEach-Object` are followed.
- Any call that names the signing key, a retired copy or `host.secret`
  is refused, reads included, with or without a flow.
- In double quotes, a backslash before anything but `$`, a backquote, `"`, a
  backslash or a newline is kept, as sh keeps it: `rm "C:\Users\me\check.sh"`
  was read as `C:Usersmecheck.sh`.
- A guarded path named through a link to the project (macOS's `/var` is one
  to `/private/var`) is named from the project, not as an absolute path.

Replaying this host's transcripts (117 on WSL with 4,725 calls, 755 on
Windows with 8,057) refused nothing new.

The guard refuses far fewer ordinary shell scripts: run as `bash run.sh`
inside a flow, 41 of 656 real scripts found on this host are refused
instead of 116, and none newly. What remains is code built from text-making
programs, `curl | sh`, git commands git cannot judge, whole-tree changes and
the scripts that do change a guarded file. A command substitution is now
judged by the program that prints it:

- Text-making programs (`printf`, `echo`, `base64`, `sed`, `cat`, `curl`,
  shells, interpreters, ...) stay strict: names built from their output
  match every protected path, code built from it is refused.
- Any other program's output is treated like an unset variable: `rm
  "$(cmd)"`, `eval "$(ssh-agent -s)"` and `. "$(git --exec-path)/git-sh-setup"`
  are no longer refused; `rm "$(cmd)/check.sh"` still is.
- `dirname`, `basename`, `pwd`, `realpath`, `readlink -f` and `cd X && pwd`
  are evaluated, with `$0` and `$BASH_SOURCE` known in scripts read from
  disk, so `cd "$(dirname "$0")" && rm -rf tests` is still refused.
- Interpreter code that names a protected directory without removing,
  moving or writing files is no longer refused; protected files still count
  wherever they are named.
- Misreads fixed: bash `[[ .. ]]`, `${..}` holding `>` or `#`, `{fd}>`
  redirections, arrays, extglob, `trap` code, `case` patterns read as
  commands (`patch)`), and an apostrophe in a `#` comment that hid the git
  commands after it (`git reset --hard # Might nuke`).

The guard corpus has 260 rows.

- macOS: a process a step or its proof starts that leaves its process group
  is stopped and leaves the step's claims unverified, as on Linux. flot
  looks at the processes below each running command every 20 ms and reads
  `FLOT_COMMAND_ID` from their environment; one that clears its environment
  and leaves its parent within 20 ms is still not seen.
- Linux: a process a step starts in the background, caught by flot inside
  `execve` with its environment not yet readable, was taken for one without
  `FLOT_COMMAND_ID`: it was stopped while its step still ran and charged to
  every running step. flot now looks again for up to 50 ms before deciding.
  Found by CI on main, where the escape tests failed once; reading the
  environment of adopted processes in a loop showed 1 empty read in 134,141.

## 0.2.7

Five ways a cached step reported a verdict its proof would not give on the
tree as it is:

- A step answers for the files it changes. The receipt records every file
  the step created, edited or deleted other than its outputs (`changed`, left
  out of receipts of steps that changed nothing, whose bytes are as before),
  and is reused only while each is still as the step left it
  (`edit: src/lib.txt changed since its receipt; the step runs again`).
  Before, a step that wrote `src/lib.txt` was reused after the file was
  changed back, and the run `PASSED` while its proof failed. The cost is
  re-runs: an agent step runs again, and is paid for again, after a person
  edits a file it changed. Every step now reads the project before and after
  it runs, as `may_change` did; what git ignores is not read, so such a
  product must be declared in `outputs`. Under `--jobs` above 1, a file
  changed beside a step is recorded in its receipt too, so receipt ids of
  steps that run beside an editing step depend on timing.
- A candidate's copy matches patterns by the project's ignore rules. The
  store's `.gitignore` ignores everything, so in the copy a wildcard input or
  proof input matched nothing: a run whose candidate rewrote the check its
  proof reads `PASSED`, and honest candidates ran again on every run. Now
  the first is `UNVERIFIED` (`the step changed its own proof`) and the
  second is reused.
- On reuse, output patterns are matched again: a file that matches one but
  is not in the receipt makes the step run again. Before, after a new
  `out/bad.txt` appeared under `outputs = ["out/*.txt"]`, the receipt was
  reused and the run `PASSED`, though the proof rejects that file.
- A failure `allow_failure` tolerated is never reused: a receipt stands in
  only if the step exited zero. Before, the next run reused the tolerated
  failure after its cause was fixed.
- `always = true` runs a step on every run and never reuses it, for a probe
  of what no key covers: the environment, `PATH` and the tools on it, which
  `docs/flows.md` now says are not keyed. Its receipt id follows its output,
  so the steps that need it are reused while that stays the same. It is
  refused with `candidates`. This repository's flow starts with such a
  `toolchain` step (`rustc -vV && cargo -V`), which the other steps need.

The guard, which refuses an agent's change to what proves its work, let
most ways of making one through. Of 75 probes (command lines, paths and
hook payloads), 61 got through before, 2 of them by failing open; 2 get
through now: `python3 -c`, which no reading of a command line can see, and
`git stash` before the first commit, which git refuses.

- `flot guard --command` reads control flow (`if`, `for`, `while`, `{ }`,
  `!`), wrappers (`sudo -u x`, `timeout`, `env`, `nice`, `busybox`), code
  run from a string (`eval`, `bash -c`, `su -c`, `$(…)`, a here-document or
  `echo … |` fed to a shell, `xargs`), `cd`, `pushd` and `git -C`, and more
  programs that write (`ln`, `install`, `yq -i`, `gawk -i inplace`,
  `sort -o`, `patch`, `rsync`, `tar`, `unzip`, compressors, `curl -o`,
  `wget`, `find -delete`/`-exec`, `git config`). Before,
  `if true; then rm check.sh; fi` and `cd tests && rm accept.sh` were
  allowed.
- `flot guard --powershell --command` reads past common parameters
  (`Remove-Item -ErrorAction SilentlyContinue check.sh` named
  `SilentlyContinue`), paths piped in, `FileSystem::` paths, every
  `[System.IO]` method that writes, `cmd /c` (`del`, `rd`, `copy`, `xcopy`,
  `robocopy`, `move`, `ren`, `>`), and code run by `iex`,
  `powershell -Command` and `pwsh -c`.
- Words are expanded as the shell would (`{a,b}`, `~`, `$HOME`, `$PWD`,
  `*`, `?`, `[…]`); a path is followed through symbolic links and matched as
  a hard link; a directory is refused when it holds a protected file
  (`rm -rf tests`, `cmd /c rd /s /q tests`). Before, `sed -i 's/1/0/' *.sh`,
  `mv check.sh{,.bak}`, `rm "$PWD/check.sh"`, a write to a link to a proof
  and `rm -rf tests` were allowed.
- A git command that changes the whole tree (`stash`, `reset --hard`,
  `checkout` or `switch` to a revision, `apply`, `am`, `merge`, `rebase`,
  `cherry-pick`, `revert`, `clean`) is put to git, and refused when git says
  it would change a protected file or cannot say (`git pull`, a patch on
  standard input, an interactive rebase). Before, `git reset --hard HEAD~1`
  and `git apply` were allowed.
- The `flot` commands that change whose evidence counts are refused, for
  the user to run: `init --force`, `trust add`/`remove`/`rotate`,
  `key init`/`rotate`, `sign`, `import`, `pull`, `gc` (not `--dry-run`),
  and `run`, `verify` or `diff` with `--proofs-from` or `-f`.
- The plugin's hook runs before every tool call (matcher `.*`, was the six
  edit and shell tools). Another tool is judged by the paths its input
  names, unless it is named like a read: `mcp__filesystem__write_file` on a
  proof was let through. Where a flow is, a call the guard cannot judge is
  denied: a payload that was not JSON made `flot guard --hook` exit 3, and
  the hook's `|| true` let the call through.
- Every flow from the current directory and from the path up to the
  repository's top is consulted, so a `flot.toml` created below a flow no
  longer hides it, and a new one inside a flow's project is protected; so
  are `.claude/settings*.json` at any depth, `~/.claude/plugins` and the flot
  binary.
- The plugin's skill tells Claude to run the whole flow, act on `result`,
  and stop and tell the user when the hook refuses a call.

A run's result:

- A run of some of a flow's steps is not a pass. `flot run --only lint` on a
  flow whose `test` step fails printed `PASSED` and exited 0, and so did
  `flot verify`. Now the run records its selection (`partial`: `selected`,
  `of`; a signature covers it, and a run of every step records none, so its
  bytes are as before), reads `PARTIAL`, and `flot run`, `flot show` and
  `flot verify` exit 2. A selected step that fails is still `FAILED`.
- `flot run --json`, `flot verify --json` and MCP `flot_run` and
  `flot_verify` carry `result` (`passed`, `partial`, `unverified`, `failed`,
  `broken`), the judgment the exit code gives. MCP `flot_verify` on a store
  whose objects were altered replied `"verdict": "passed"` with
  `isError: false` while `flot verify` exited 1; it now replies
  `"result": "broken"` with `isError: true`.
- MCP `flot_run`'s `max_cost_usd` can only lower the flow's
  `max_cost_usd`. A call passing `100000` ran past a flow ceiling of $0.50;
  now the run keeps the $0.50 ceiling.

Proofs and processes:

- A run without `proofs_from` says that its proofs are held to no revision,
  and names a step whose proof (its files, its `verify` command or its
  claims) changed since the step's last run here. Before, a run after
  `check.sh` was weakened to `exit 0` read `PASSED` with nothing to say so.
- `flot init` in a clone whose `origin` names a default branch sets
  `proofs_from` to it; elsewhere it writes the setting commented out.
- On Linux, a process a step or proof starts that leaves its process group
  is stopped once that command ends, and the step's verified claims stay
  unverified. Before, a step that left behind
  `setsid sh -c 'sleep 1; echo 0 > out.txt'` read `PASSED` with `out.txt`
  holding 1, and the file held 0 a second later.
- A sandbox with `write` is not run on Landlock below ABI 3 (Linux 6.2),
  and `flot doctor` fails there. Before, such a step ran without truncation
  denied, so it could empty any file it could open.
- `flot doctor` warns of a proof that declares no `verify_inputs`.

Sharing:

- `flot bundle` and `flot push` also refuse Slack `xoxs-` and `xapp-`,
  Stripe `sk_live_` and `rk_live_`, Hugging Face `hf_` and PyPI `pypi-`
  tokens, and a password in a URL (`scheme://user:password@`, shown as
  `://user:…@`; one that starts with `$`, `<`, `{` or `*` is taken for a
  placeholder).

- `flot verify` prints the run's limitations under its result, as
  `flot run` and `flot show` do; before, a run whose proofs were held to no
  revision verified as `verdict passed` with nothing to say so.

This repository:

- The flow's `format` step runs `cargo fmt --check` rather than
  `cargo fmt`, so it checks the files its proof judges instead of rewriting
  them.
- `rust-toolchain.toml` pins the toolchain (1.100.0), and CI and the release
  read it from there; the MSRV job keeps 1.88. With the `toolchain` step, a
  run signed on a host with another rustc was not reused in CI: PR CI
  45736380180 ran every step under 1.100.0 against a run signed under 1.99.1.

## 0.2.6

- `flot verify` names a run signed by this host's own key, while that key is
  not on the trust list, as `authorship: this host's key` (`this_host` in
  `--json`, with `this_host: true` on the signer) instead of `untrusted`. It
  is a label only: `--require-trusted`, import and cache reuse still count
  only keys on the trust list, and another host reading the run still sees
  `untrusted`.
- `examples/agent-review.toml` proves an implementation with
  `cargo fmt --check`, clippy and a unit test the change adds, not only a
  build and the existing suite, and its spending caps sit above what a run
  with no cap spent ($6.81 for the three steps). The run, and what it got
  wrong, is in `docs/agents.md`.

## 0.2.5

- The guard reads PowerShell: the Claude Code hook now covers the
  `PowerShell` tool, `flot guard --replay` judges its calls, and
  `flot guard --powershell --command CMD` reads a command line as PowerShell
  (redirections, cmdlets that change files and their aliases, `[IO.File]`
  methods, `Invoke-WebRequest -OutFile`, and downloaded code handed to
  `Invoke-Expression`).
- `sandbox` is enforced on macOS too: the step's shell starts under
  `/usr/bin/sandbox-exec` with a Seatbelt profile that denies every write
  outside the allowed paths and, without `network`, every network operation.
  The receipt's mechanism reads `seatbelt file writes, seatbelt network`.
  Elsewhere than Linux and macOS a confined step is still not run.
- Where git does not trust the file system's executable bit
  (`core.fileMode` false: Git for Windows, WSL's `/mnt` drives), an input git
  tracks is keyed by the bit git records, as a Linux or macOS checkout keys
  it. A run on such a drive could never be reused in CI before; a candidate's
  copy is keyed by the workspace's records too.
- This repository's CI trusts the signers in `.github/flot-signers` on the
  base branch, pulls their runs and reuses them; `docs/ci.md` shows how.
- Reports name who vouched for a step reused from an imported signed run.
  The run records the signers' keys on the step (`vouched_by`); `flot run`
  and `flot show` end the step's line with `vouched for by NAME`,
  `flot verify` lists such steps, and `flot verify --markdown` marks the step
  `ok, reused, vouched for by NAME` and adds a line
  `Reused from signed runs: N step(s), vouched for by NAME`; `--json` of
  `run`, `show` and `verify` carries them as `vouched`. That record is the
  run's own word, and CI's run is unsigned, so a signer is named only when
  this store holds a run the signer validly signed that kept the same receipt
  for the step, and only while the trust list holds the key. Otherwise the
  key reads `ed25519:<first 16 hex> (not confirmed here)` or
  `(not trusted here)`. A run that names signers for a step it did not
  reuse, a key twice, or a key not spelled as flot writes keys does not
  authenticate.
- Reusing an imported receipt no longer remembers it as this host's own
  work: its cache entry keeps the signers, so removing a signer from the trust
  list stops the reuse on later runs too, not only on the first. Every
  earlier release did; an entry one wrote this way has no signers and is
  still reused as this host's work after its signer is removed. Run
  `flot run --force` once after removing a signer: each step that passes
  replaces its entry with this host's own result.
- An import replaces a cache entry that only signers this store no longer
  trusts vouched for, so a trusted signer's run of the same work is reused
  instead of being executed again.
- A step whose inline output was trimmed no longer adds a limitation saying
  so: trimming is how the output is shown, not a limit of the evidence, and
  the `complete output: flot log ...` line under the step already says where
  the rest is.

## 0.2.4

- Release archives carry a GitHub build provenance attestation, made by the
  release workflow for the tag. The action checks it with
  `gh attestation verify` (signer workflow and tag enforced) on top of the
  `.sha256`, so it installs 0.2.4 or later.
- The Claude Code hook notes each denial in the store (only where one
  exists), and the next `flot run` records those made since the previous run
  finished in its manifest as `refusals` (tool, reasons, time), covered by a
  signature. `flot run`, `flot show` and `flot verify --markdown` list them.
- `flot guard --replay [PATHS]` judges every `Edit`, `Write`, `MultiEdit`,
  `NotebookEdit` and `Bash` call in Claude Code transcripts (by default
  `~/.claude/projects`) as the hook would now and lists what it would refuse;
  `--json` prints the counts and the refusals.
- `fails_before = true` on a step: once its proof passes, it runs again on a
  copy of the work (what git sees, plus the step's declared files), where it
  must pass, and then there with every change since `proofs_from` undone
  (edited and deleted files back as at the revision, new files removed, what
  git ignores left alone). The claims verify only if it fails there; a proof
  that passes either way, or that cannot pass on the copy, leaves them
  unverified. The result is kept as the claim's `before` record (commit,
  exit code, output), `flot show` prints it, a cached receipt is reused only
  if it was checked at the current revision, and `flot verify` treats a
  dropped `fails_before`, or a verified claim not checked at the revision, as
  a proof that differs. Not available with `candidates`.
- `flot flaky [--runs N]` names steps that were `ok` and `failed` on one key,
  and proofs that were `verified` and `refuted` over one step key and one set
  of proof files, in the newest runs executed here; exits 2 when there is one.
  `flot doctor` warns about them.
- Held to a revision, `flot verify` (text, `--json` as `anchor.unread`, and
  `--markdown`) names the files that differ from it and that no step read,
  checked or produced: the changes no claim speaks for. The verdict and exit
  code are unchanged.
- flot's own flow sets `proofs_from = "origin/main"`.
- `flot doctor` checks what the evidence relies on without changing
  anything: the flow parses, `proofs_from` is set and resolves, git ignores
  the store, the signing key and trust list, that a requested sandbox can be
  enforced (by confining a trivial child), and Claude Code settings that set
  `disableAllHooks`, `bypassPermissions` or hold a credential. Exits 1 when a
  check fails.
- `flot guard --command CMD` reads a shell command line for the files it
  writes (redirections, `rm`, `mv`, `cp`, `tee`, `sed -i`, `git checkout`, ...)
  and refuses `curl … | sh`. The Claude Code hook now also runs before `Bash`.
  `flot guard --json` prints `{protected, refused}`.
- `flot guard`, and so the Claude Code hook, also protects what decides
  whether proofs are checked: `.claude/settings.json` and
  `settings.local.json`, `.github/workflows/`, `.git/hooks/`, `.git/config`
  (in the flow's repository) and `~/.claude/settings.json`.
- `flot bundle` and `flot push` refuse a run that carries a credential-shaped
  token (GitHub, GitLab, npm, Slack, Anthropic, OpenAI, AWS, Google, PEM
  private keys) in its output, receipts or produced files, naming the step and
  showing only the prefix. Nothing is written or signed first.
  `--allow-secrets` shares it anyway.
- The action no longer takes `signers`: it was never run in CI, and whom CI
  trusts is better written out in the workflow; `docs/ci.md` shows how.

## 0.2.3

- `attempts = N` on a step: one run executes it up to N times until it holds,
  and each later attempt gets `$FLOT_FEEDBACK`, a file saying why the previous
  one did not (verdicts, limitations, the end of the proof's and the command's
  output). Earlier attempts are kept as evidence: the manifest lists them,
  `show` names them, and `verify`, `bundle`, `import` and `gc` treat them like
  any other receipt; their spend counts toward the run and its ceiling.
- `examples/slugify`: the implementer has two attempts with feedback, and can
  no longer read the acceptance check, which a real run showed it did.
- `candidates = [...]` on a step: one run executes it once per candidate
  (`$FLOT_CANDIDATE` names it), each in its own copy of the project inside the
  store, proves each, and keeps the first that held, by known and then lower
  cost, then by order; its outputs are written back. The manifest lists the
  candidates not kept, `flot show` names them, `verify` checks the choice
  again from the recorded spend, and `bundle`, `import` and `gc` keep their
  receipts.
- `flot log STEP --candidate NAME --attempt N`, and the same arguments on
  `flot_step_output`, read a candidate not kept or an attempt that did not
  hold.
- `flot search QUERY` and the `flot_search` MCP tool: a literal, case-blind
  search of recorded runs, newest first, over each executed step's complete
  output, its proofs' output, its claims and their reasons and limitations,
  including attempts and candidates not kept. Each hit names its run, step,
  candidate, attempt, source and line.
- `flot push [RUN] [--remote R] [--sign]` and `flot pull [--remote R]
  [--require-trusted]` share runs through a git remote, as bundles in the ref
  `refs/flot/runs`; flot runs `git` and has no network code of its own. A
  pulled run is imported like `flot import`, so only a trusted signer's
  receipts stand in for local work.
- A Claude Code plugin (`plugins/claude-code`, with this repository as its
  marketplace): it registers `flot mcp` and adds a skill that runs the flow
  before Claude says a change works.
- `proofs_from = "REV"` in `[flow]` and `--proofs-from REV` on `run` and
  `plan`: each step's proof (claims, `verify`, `verify_inputs` and the files
  they match) must be as it is at a git revision such as `origin/main`, or the
  proof is not run, its claims stay unverified and no receipt stands in. The
  flow as committed at `HEAD` decides the revision before the working copy
  does, and the MCP tools take none.
- `flot verify --proofs-from REV` holds a recorded run's proofs to a revision,
  and exits 2 when one differs or a claim there is missing from the run.
- `flot guard PATH...` names the paths whose change would touch the proofs (the
  flow file, files `verify_inputs` matches, the store); `--hook` answers a
  Claude Code `PreToolUse` payload, and the plugin installs it for file edits.
- `may_change = [...]` on a step: a step that creates, edits or deletes a file
  outside those globs and its `outputs` (and not ignored by git) fails, naming
  each file. Such a step runs with no other step beside it, and the scope is
  part of its key.
- `flot verify --markdown`: the check as Markdown for a pull request comment
  or a job summary, with the headline the exit code gives; claims and
  limitations are escaped so that they render as text.
- A GitHub action (`action.yml`): installs a release binary, runs the flow,
  holds proofs to the base branch of a pull request, reports in the job
  summary and in one pull request comment, and fails the job unless
  `flot verify` exits 0. With `signers`, it trusts the keys a file on the base
  branch lists and pulls their signed runs first. See `docs/ci.md`.
- `sandbox = { network = false, write = [...] }` on a step, on Linux: the
  step runs in a new user and network namespace and under a Landlock ruleset
  that denies every write outside the named paths (and its own scratch
  `$TMPDIR`). Where that cannot be enforced, the step is not run. The receipt
  records the policy and the mechanism.
- A metrics report without `cost_usd` is an unknown cost, not $0: `flot run`
  counts such steps as "without cost", and the cost ceiling names them.
- `examples/slugify/compare.toml`: Claude Code and Codex implement the spec as
  two candidates.
- Windows support; the whole suite runs on Windows (build 26300, and
  `windows-latest` in CI):
  - steps run in the `sh` of Git for Windows (on `PATH`, else beside
    `git.exe`); a missing shell is named;
  - a step and everything it starts are held in a job object, so a timeout,
    Ctrl-C or a step that leaves work running stops all of it, including what
    would escape a POSIX process group;
  - the store lock uses `LockFileEx`, so `gc` cannot run under a writer;
  - the signing key defaults to `%APPDATA%\flot\signing.key`;
  - releases carry a Windows x86_64 binary (`.zip`).
- A receipt recorded on Linux is reused on Windows and the other way round,
  with its produced files restored, when the inputs' executable bits agree
  (Windows records none).

## 0.2.2

- A reused step prints one line in `flot run`; its output was shown when it
  ran and is still in the store.
- A step's inline output keeps its first and last lines, where builds and
  test runners report failures, instead of only its beginning. A trimmed step
  names the `flot log` command that prints all of it.
- `flot log STEP [RUN] [--stderr]` and MCP `flot_step_output` print the
  complete output of one step by its id; `flot show` points each failed step
  at it.
- `latest~N` names the run executed N runs before the latest, and `flot diff`
  (and `flot_diff`) compares `latest~1` with `latest` by default.
- A wildcard input skips what git ignores and never enters `.git`, so `**/*`
  hashes the project instead of `target/`: on this repository `flot plan` with
  `inputs = ["**/*"]` went from 13082 files in 2 minutes to 53 files in 0.2 s.
  A path the pattern spells out is still followed, and outputs still reach
  ignored build products.
- `flot run` and `flot show` print a limitation several steps share once,
  naming the steps, instead of once per step.
- This repository's own gate declares the READMEs as inputs of its test step:
  `tests/readme.rs` reads them, and a README edit left a cached pass in place.
- A new `.flot` store writes its own `.gitignore`, so `git status` stays clean.
- `flot_run` tells agents that a cached step is as strong as a fresh run, so
  they stop forcing re-runs to double-check it.

## 0.2.1

- `flot key rotate` replaces the signing key and writes a handover that the
  old and the new key both sign; the old secret is kept owner-only beside the
  new key. If the handover cannot be written, the key is not replaced.
- `flot trust rotate HANDOVER [--revoke]` follows a trusted signer to their
  new key under the same name. A handover from a key this store does not trust
  is refused, and one whose signatures do not verify is refused as altered.
  The old key stays trusted unless `--revoke`.
- `flot trust remove NAME` asks for the key when the name stands for two.
- The README is also available in Chinese; every release updates the
  English, Korean and Chinese READMEs together, and a test checks that each
  names the release.

## 0.2.0

- `flot sign [RUN]` and `flot run --sign` sign a run executed here without
  exporting it; the attestation is kept in the store, so `flot verify` reports
  it and every later bundle carries it. A run that was imported, or whose
  evidence no longer authenticates, is never signed; `flot bundle --sign` now
  refuses the latter too. `run --sign` checks for the key before anything runs.
- Releases ship prebuilt binaries for Linux (x86_64, aarch64; static) and macOS
  (x86_64, arm64) with checksums, built and published by CI.

## 0.1.1

- MCP tools carry annotations: every tool but `flot_run` is read-only, so hosts
  such as Codex let them through without asking. `flot_run` is marked
  destructive and open-world, since it runs the flow's unsandboxed commands.
- The cost-ceiling limitation names the steps that reported no metrics.
- Examples run each agent step apart from the user's own Claude Code setup
  (`--setting-sources project`, `--strict-mcp-config`, only the tools the step
  needs), which cut a three-reviewer run from $3.25 to $0.42.
- `examples/parallel-review.toml` reviews staged and unstaged changes
  (`git diff HEAD`).
- Documentation: budget defaults, MCP approvals, and runs tested with Claude
  Code and Codex.

## 0.1.0

First release.
