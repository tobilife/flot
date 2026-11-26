# Changelog

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
