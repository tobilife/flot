# Changelog

## Unreleased

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
