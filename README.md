<div align="center">

**English** · [한국어](docs/readme/README_ko.md) · [中文](docs/readme/README_zh.md)

# flot

### Verifiable flows for coding agents.

**An agent says it is done. flot keeps the receipt that proves it.**

*flot = **flo**w + bo**t***

[![CI](https://img.shields.io/github/actions/workflow/status/tobilife/flot/ci.yml?branch=main&style=flat-square&logo=github&label=ci)](https://github.com/tobilife/flot/actions/workflows/ci.yml)
[![MIT license](https://img.shields.io/badge/license-MIT-f4c430?style=flat-square)](LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-dea584?style=flat-square&logo=rust)](Cargo.toml)
[![Linux | macOS | Windows](https://img.shields.io/badge/Linux%20%7C%20macOS%20%7C%20Windows-4493F8?style=flat-square)](docs/architecture.md#current-scope)
[![MCP](https://img.shields.io/badge/MCP-stdio-8A2BE2?style=flat-square)](docs/mcp.md)

[Why and when](#why-and-when-to-use-flot) · [Quick start](#quick-start) · [How flot works](#how-flot-works) · [What flot guarantees](#what-flot-guarantees) · [Example flows](#example-flows) · [FAQ](#faq) · [Documentation](#documentation)

<code>cargo install flot-cli</code>

Latest release **0.2.2**: long step output shows its first and last lines, `flot log` prints all of it, `flot diff` compares the last two runs, and wildcard inputs skip what git ignores · [changelog](CHANGELOG.md)

</div>

---

Coding agents report success in prose: "implemented, all tests pass". flot
turns that into evidence. You declare the work as a small flow of steps (shell
commands, test suites, agent CLIs), and flot runs it, records a
content-addressed **receipt** for every step, and checks every claim with a
command the agent did not write.

One rule is enforced by the runtime, not by prompt text:

> **A claim is unverified until an independent command proves it.**
> A run with unproven claims can never report `passed`.

Receipts are reused when nothing they depend on changed, so a second run costs
nothing. They can be bundled, signed and handed to a reviewer on another
machine, who can check them without trusting the agent or its host. flot is one
binary with one `flot.toml` and one `.flot/` directory, usable from your
terminal and from any agent that speaks MCP.

## Why and when to use flot

An agent's "done" is a claim about your code, and checking it by hand every
time does not scale: you rerun the tests, scroll through its log, and still
cannot show anyone else what passed. flot makes that check part of the work.
The agent runs the flow, the flow proves each claim, and what passed is
recorded where you, your CI and your reviewers can check it again.

It pays off when:

- **An agent fixes or builds something and says it works.** The verdict comes
  from commands the agent did not write. In a real session, Claude Code was
  given a failing flow in a fresh clone of `itoa`, found the failing tests in
  the recorded output, fixed the one wrong digit and got `passed` from
  `flot_run`, for $0.42.
- **An agent loops: change, check, change again.** Steps whose inputs did not
  change are reused, so the check is cheap to repeat: this repository's gate
  takes 16 s cold and 0.41 s when nothing changed. A step with `attempts = N`
  runs again with what its proof said, until it holds: in a real run the
  check's failure output led Claude Code to a rule the spec never stated, on
  the second attempt, for $0.25.
- **Several agents work on one change.** Reviewers run concurrently under a
  cost ceiling, and each one's spend is recorded: isolating the three
  reviewers in `parallel-review` cut a run from $3.25 to $0.42.
- **You want to know which agent does a task best.** A step with
  `candidates` runs each agent or model in its own copy of the project, proves
  each one with the same check, and keeps the cheapest that held: in a real
  run Claude Code and Codex both passed, and Claude was kept at $0.10 because
  Codex reported tokens but no cost.
- **Someone else has to trust the result.** `flot bundle` gives a reviewer or
  another machine one file to import and verify, and `flot push` and
  `flot pull` share runs through a git remote you already use; signatures and
  the reader's own trust list decide whose receipts count.
- **A pull request must not merge on a weaker check.** The GitHub action
  runs the flow in CI, holds every proof to the base branch, and puts the
  report in the job summary and on the pull request; as a required check it
  blocks the merge.
- **You want to know what changed between two runs.** `flot diff` names each
  step whose receipt or status changed, and `flot search` finds the runs whose
  output, claims or limitations mention a message, newest first.

It is not the tool when:

- you run a command once and need no record of it;
- you need to contain untrusted code: flot records and judges steps, it does
  not sandbox them;
- you want a CI service: flot runs inside your CI or your terminal, it does not
  replace them.

## Quick start

### Install

With [Rust](https://rustup.rs) 1.88 or later:

```bash
cargo install flot-cli
```

The crate is `flot-cli`; the command it installs is `flot`. Prebuilt binaries
for Linux (x86_64, aarch64) and macOS (x86_64, arm64) are on the
[releases page](https://github.com/tobilife/flot/releases/latest). For the
latest `main` instead of the latest release, use
`cargo install --git https://github.com/tobilife/flot`.

### Connect your agent (MCP)

```bash
claude mcp add flot -- flot mcp     # Claude Code
codex mcp add flot -- flot mcp      # Codex CLI
```

In Claude Code you can install the plugin instead: it registers the same
server, adds a skill that runs the flow before Claude says a change works, and
adds a hook that refuses edits to the flow file and the files its proofs read.
flot must be on your `PATH`.

```text
/plugin install flot --marketplace tobilife/flot
```

In a real run, the prompt "The slug tests fail. Fix slug.py and tell me when
the work is done.", which does not mention flot, led Claude Code 2.1.330 with
the plugin to load the skill, call `flot_run` and report `passed`, for $0.24.
Asked to replace a proof with `true`, Claude was refused by the hook and said
so, for $0.23.

Restart the agent in a project that has a `flot.toml` (`flot init` writes a
starter one). Then try asking it:

```text
Fix the failing test. When you are done, run flot_run and show me the verdict
and any claim that is not verified.
```

The agent gets tools to plan, run, show, verify, compare and search runs. A run whose
claims were not proven comes back as `unverified`, not as a tool error, so the
agent cannot mistake a missing proof for a glitch. Every tool but `flot_run`
is marked read-only; a headless agent needs `flot_run` approved in advance, as
[Approvals](docs/mcp.md#approvals) shows. See [MCP server](docs/mcp.md) for
every tool and its contract.

### Use flot from the terminal (CLI)

```bash
flot init        # write a starter flot.toml
flot plan        # what would run, what would be reused, which claims lack proof
flot run         # execute and record receipts
flot show        # steps, claims and limitations of the latest run
```

This repository gates itself with flot ([`flot.toml`](flot.toml)):

```
$ flot run --force
ran      format               ok        183ms  claims 1v/0u/0r  receipt 7c8c480702e0
ran      build                ok        876ms  claims 1v/0u/0r  receipt 2f5273d0f500
ran      lint                 ok        816ms  claims 1v/0u/0r  receipt 48a64dd4723f
ran      test                 ok        5657ms  claims 2v/0u/0r  receipt 3a0fd0619f68
    complete output: flot log test 1795521673839-85aac97d67a6

PASSED 1795521673839-85aac97d67a6  root 85aac97d67a6  16144ms
limitations:
  - test: inline output trimmed to 2048 bytes per stream; complete streams are in the store

$ flot run     # nothing changed: receipts are reused and the root is identical
cached   format               ok        183ms  claims 1v/0u/0r  receipt 7c8c480702e0
cached   build                ok        876ms  claims 1v/0u/0r  receipt 2f5273d0f500
cached   lint                 ok        816ms  claims 1v/0u/0r  receipt 48a64dd4723f
cached   test                 ok        5657ms  claims 2v/0u/0r  receipt 3a0fd0619f68

PASSED 1795521691818-85aac97d67a6  root 85aac97d67a6  412ms
```

Recorded with flot 0.2.2. The second run took 0.41 s instead of 16 s because
all four receipts were reused; a reused step reports the duration it was
recorded with and prints nothing else. Under each step that ran, flot also
prints the first and last lines of its output, left out here; `flot log test`
prints all of it.

`1v/0u/0r` counts verified, unverified and refuted claims. See the
[CLI reference](docs/cli.md) for every command, run selectors, `--json` and
exit codes.

## How flot works

```mermaid
flowchart LR
    you["You or your agent"] -->|flot run / flot_run| flot
    flot -->|runs| steps["Steps<br/>shell · tests · agent CLIs"]
    flot -->|then proves each claim with| proofs["verify commands"]
    flot -->|records| store[("Receipts<br/>.flot/")]
    store -->|flot bundle --sign| reviewer["Reviewer on another host<br/>flot import · flot verify"]
```

A step declares what it runs, what it reads, what it asserts and how that is
proved:

```toml
[flow]
name = "release-candidate"
budget = { timeout_secs = 900 }

[[step]]
id = "implement"
run = "claude -p 'implement .flot-work/spec.md'"
needs = ["spec"]
inputs = [".flot-work/spec.md"]
outputs = ["src/**/*.rs"]
claims = ["the workspace still compiles after the agent's edits"]
verify = "cargo build --all-targets --quiet"
```

flot derives a **step key** from the command, the content of every input, the
proof and the receipts of upstream steps. If the key is unchanged, the earlier
receipt is reused and its output files are restored. Otherwise the step runs,
its proof runs after it, and a new receipt is sealed: the receipt id is the
digest of what happened, so an edited receipt stops authenticating. The run's
verdict is `failed`, `unverified` or `passed`, in that order of precedence.

[Writing flows](docs/flows.md) covers every field: keys, receipts, claims,
budgets, concurrency, outputs, guarded proofs and scope.

## What flot guarantees

- **No proof, no pass.** A claim is `verified` only when its proof command ran
  after the step succeeded and exited zero. A failed, timed-out, killed or
  tolerated step proves nothing, and a proof whose files changed while it ran
  does not count.
- **Proofs the agent cannot rewrite.** With `proofs_from = "origin/main"`, a
  proof that differs from that revision (its command, its claims or the files
  it reads) leaves its claims unverified, and `flot verify --proofs-from`
  holds a recorded run to the same revision. The Claude Code plugin also
  refuses edits to those files before they happen.
- **Changes kept in scope.** A step with `may_change = ["src/**"]` fails if
  it creates, edits or deletes any other file that git does not ignore.
- **Receipts you can check.** Every receipt re-hashes to its own id, and a run's
  verdict and root digest are recomputed from its receipts, never read.
  `flot verify` re-reads every stored byte.
- **Reuse you can trust.** A cache hit requires the same command over the same
  input content and the same upstream receipts, sealed by this host. Failures
  are never cached.
- **Evidence that travels.** `flot run --sign` signs each run as it is
  recorded, and `flot bundle` produces one file a reviewer can import and
  verify. Imported receipts stand in for local work only when a
  signer the reviewer trusts signed them, and only while that trust lasts.
  A new key comes with a handover both keys sign (`flot key rotate`), which
  readers follow with `flot trust rotate`.
- **Bounded runs.** Timeouts and output limits stop work and say so; a cost
  ceiling stops starting steps once agents have reported that much spend.
- **One contract.** The CLI and the MCP server call the same functions, so a
  human and an agent see the same receipts, verdicts and digests.

[Evidence, bundles and trust](docs/evidence.md) explains signing, the trust
list and the `.flot` store.

## Example flows

| Flow | What it shows |
|---|---|
| [`examples/slugify`](examples/slugify/flot.toml) | One agent implements a spec, a check no agent wrote proves it, a second agent reviews. Run end to end against Claude Code; [the full story](docs/agents.md#a-real-run-end-to-end) includes the bug it caught in flot itself. With isolated agent steps a fresh run costs $0.48. |
| [`examples/slugify/compare.toml`](examples/slugify/compare.toml) | Claude Code and Codex implement the same spec as two candidates; the same check proves each, and the cheapest that held is kept. [One real run](docs/agents.md#two-agents-on-the-same-spec) and the two bugs it found. |
| [`examples/agent-review.toml`](examples/agent-review.toml) | A spec, an implementation and a review as three agent steps; the project's tests prove the implementation. |
| [`examples/parallel-review.toml`](examples/parallel-review.toml) | Three independent agent reviewers (security, performance, contracts) of the current diff, run concurrently, then a test gate. [One real run](docs/agents.md#three-reviewers-in-parallel) found an undocumented default; isolating the agent steps cut a run from $3.25 to $0.42. |
| [`flot.toml`](flot.toml) | This repository's own gate: format, build, lint and test, each with a proof. |

Copy an example to your project root before running it: paths and the `.flot`
store are relative to the flow file. [Running coding agents](docs/agents.md)
covers agent steps and spend accounting.

## FAQ

<details>
<summary><strong>Is flot a plugin for Claude Code or Codex?</strong></summary>

No. flot is a standalone binary. It works with any agent in two directions: the
agent calls flot through MCP (`flot mcp`), or flot calls the agent as a step
(`claude -p …`, `codex exec …`). It installs no hooks, prompts or agent
definitions; it judges work from outside the agent.

</details>

<details>
<summary><strong>Which agents can use flot?</strong></summary>

Any agent that supports local MCP servers over stdio, and any agent with a
command-line interface for use inside steps. Registration for Claude Code and
Codex is shown above; see [MCP server](docs/mcp.md) for the protocol details.

</details>

<details>
<summary><strong>Does flot sandbox the steps it runs?</strong></summary>

No. A step runs with your user's permissions and environment. flot records and
judges what steps did; it does not confine them. Run flows you have read, and
an agent with the permissions you would give it anyway.

</details>

<details>
<summary><strong>Why is my run <code>unverified</code>?</strong></summary>

At least one claim has no proof that settled it: the step has no `verify`
command, the step failed or was tolerated with `allow_failure`, or the proof
timed out, was killed, or saw its files change. `flot show` names each
unverified claim, and `flot plan` lists claims that lack a proof before you
run anything.

</details>

<details>
<summary><strong>Does flot send my code anywhere?</strong></summary>

No. flot runs locally and has no network code. `flot push` and `flot pull`
run `git` to share runs with a remote you name, and only when you run them.
Agents you run as steps talk to their own providers under their own data
policies.

</details>

<details>
<summary><strong>Does it work on Windows?</strong></summary>

Yes, with [Git for Windows](https://gitforwindows.org) installed: flows are
written for `sh` on every platform, and flot runs steps in the one Git for
Windows provides. A step and everything it starts are held in a Windows job
object, so a timeout or Ctrl-C stops all of them. The test suite runs on
Windows. Install with `cargo install flot-cli`. Windows has no signals and no
executable bit: a killed proof is judged by the exit code it was given, and a
file that is executable on Linux or macOS keys differently there, so such
receipts are not reused across the two.

</details>

<details>
<summary><strong>What should I do if I hit a bug?</strong></summary>

Update with `cargo install flot-cli` and retry. If it persists, [open an issue](https://github.com/tobilife/flot/issues)
with `flot --version`, the flow file, the command you ran and its output.

</details>

## Documentation

- [Writing flows](docs/flows.md): steps, keys, receipts, claims, attempts, candidates, budgets, concurrency, outputs, guarded proofs and scope.
- [In CI](docs/ci.md): the GitHub action, the pull request report, and reusing a developer's signed run.
- [CLI reference](docs/cli.md): commands, selecting runs and flows, `--json` and exit codes.
- [MCP server](docs/mcp.md): registration, tools, replies, errors and cancellation.
- [Running coding agents](docs/agents.md): an end-to-end agent run, attempts with feedback, two agents compared, spend and the cost ceiling.
- [Evidence, bundles and trust](docs/evidence.md): bundles, sharing through a git remote, signing, key rotation, the trust list and the `.flot` store.
- [Architecture](docs/architecture.md): layers, enforced dependency rules and current scope.

Report vulnerabilities through [SECURITY.md](SECURITY.md).

## Contributing

[Open an issue](https://github.com/tobilife/flot/issues) to report a bug or
suggest a feature, or [send a pull request](https://github.com/tobilife/flot/pulls).
See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and checks.

## License

[MIT](LICENSE)
