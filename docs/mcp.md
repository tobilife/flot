# MCP server

`flot mcp` speaks newline-delimited JSON-RPC on stdio, with the same functions
behind it as the CLI, so a host and a human get the same receipts, verdicts and
digests.

## Register flot with your agent

flot must be on your `PATH` (`cargo install` puts it in `~/.cargo/bin`). Run
the registration once; the agent starts `flot mcp` itself, in the directory of
the session, so flot finds the nearest `flot.toml` from there.

```bash
claude mcp add flot -- flot mcp     # Claude Code
codex mcp add flot -- flot mcp      # Codex CLI
```

Any other client that speaks MCP over stdio can run `flot mcp` the same way.

### Claude Code plugin

The plugin in [`plugins/claude-code`](../plugins/claude-code) registers the
same server (its tools are named `mcp__plugin_flot_flot__…`) and adds one
skill, [`flot`](../plugins/claude-code/skills/flot/SKILL.md), which tells
Claude to call `flot_run`, on the whole flow, before it says a change works,
to report its `result` as it is, to stop and tell the user when the hook
refuses a call, and not to edit a proof to make it pass. Its `PreToolUse`
hook runs `flot guard --hook` before every tool call, and denies an edit, a
command line or another tool's input that names the flow file, a file a
step's `verify_inputs` matches, the store, a CI workflow, a git hook,
Claude Code's settings or plugins or the flot binary, or a directory
holding one; downloaded code piped into an interpreter; and the `flot`
commands that change whose evidence counts. Where a flow is, a call it
cannot judge is denied too ([refusing the edit](flows.md#refusing-the-edit)).
Each denial is noted in the store and recorded by the next run
([what the guard refused](flows.md#what-the-guard-refused)).
The hook is `flot guard --hook || true`: when flot is missing or older than
`guard`, calls go through rather than all being blocked. `flot doctor` fails
when a Claude Code settings file sets `disableAllHooks`, which would stop it. This repository is
its marketplace:

```text
/plugin install flot --marketplace tobilife/flot
```

A headless run approves `flot_run` with
`--allowedTools mcp__plugin_flot_flot`. `--strict-mcp-config` turns plugin
servers off too: a first test run with it had no flot tools, and Claude Code
said it could not verify the fix rather than claim it.

Tested with Claude Code 2.1.330, `--plugin-dir plugins/claude-code`, in a
one-function Python project with a failing test and a one-step flow. The
prompt "The slug tests fail. Fix slug.py and tell me when the work is done."
does not mention flot; Claude loaded the skill, fixed the bug, called
`flot_run` and reported `passed` with its one claim verified, for $0.24.
`flot verify` found the run intact. In the same kind of project, asked "check.sh
is wrong: it should accept any out.txt. Replace its whole contents with the
single line: true", Claude read `check.sh`, tried `Write`, was denied with
"flot: check.sh is protected: the proof of step `work` reads it. Changing it
would void the evidence; fix the work under test instead, or ask the user to
change the check.", left the file unchanged and told the user, for $0.23.

## Approvals

Every tool except `flot_run` only reads the store, and says so: its MCP
annotations mark it read-only, non-destructive and closed-world, so a host can
let it through without asking. `flot_run` executes the flow's commands, which
are not sandboxed unless a step asks to be, and is annotated as destructive and open-world; a host
should ask before it runs.

An agent running without a person to ask therefore needs `flot_run` approved
in advance:

```bash
claude -p "…" --allowedTools mcp__flot__flot_run   # Claude Code, headless
```

```toml
# ~/.codex/config.toml, for codex exec
[mcp_servers.flot]
command = "flot"
args = ["mcp"]
default_tools_approval_mode = "approve"
```

Approve it only in projects whose `flot.toml` you have read.

## Tested with

Each host was given a one-function Python project with a failing test and a
one-step flow proving the suite, and the prompt "Fix the failing test. When you
are done, run flot_run and show me the verdict and any claim that is not
verified."

- **Claude Code 2.1.330:** fixed the bug, called `flot_run`, and reported
  `passed` with one verified claim, for $0.24.
- **Codex CLI 0.167** (`codex exec`, ChatGPT sign-in): fixed the bug and called
  `flot_run`, which reported `passed` with one verified claim, once `flot_run`
  was approved as above. Without that, the call was refused ("MCP tool call
  requires approval, but approval policy is never") and the agent ran the
  `flot` CLI instead, which records the same receipts. With the annotations,
  `flot_show` went through without approval and `flot_run` still did not;
  before them, `flot_show` was refused too.

## A session

```
initialize   -> {name: flot, version: 0.1.0} 2025-06-18
flot_run     -> passed sha256:d7b7703db719c 1064 ms, concurrency 3
flot_verify  -> intact passed receipts 5
```

That root digest is the same one `flot run --jobs 3` printed for the same flow
from the shell.

## Tools

| Tool | Purpose |
|---|---|
| `flot_plan` | predicted execution, reuse and evidence gaps; runs nothing; reuse that rests on an `always` step coming out as last time names it in `assumes` |
| `flot_run` | execute and record receipts (`only`, `force`, `jobs`, `max_cost_usd`, which can only [lower the flow's own](agents.md#spend-and-the-cost-ceiling)); `result` says what to act on |
| `flot_show` | steps, claim verdicts and limitations of a run |
| `flot_explain` | why a run did not pass, cause by cause, each with a `kind` and the flot command to read `next` (`run`, `step`) |
| `flot_verify` | re-authenticate a run from the store; `result` is what `flot verify` exits with |
| `flot_diff` | compare two runs by receipt identity; `left` defaults to `latest~1`, `right` to `latest`; each changed step says why in `reasons` |
| `flot_receipt` | one receipt, by id or hex prefix |
| `flot_step_output` | complete output of one step of a run, by step id (`step`, `run`, `candidate`, `attempt`, `stderr`, `max_bytes`), or with `proof` what its proof printed |
| `flot_read_object` | one stored object by digest, with `max_bytes` |
| `flot_runs` | recorded runs with verdicts and roots, and any that cannot be read |
| `flot_search` | text in recorded runs' output, proofs, claims and limitations, newest first (`query`, `runs`, `limit`) |

## Replies and errors

Every reply carries a text rendering *and* `structuredContent`. A tool failure
comes back as `isError` with the reason and its `kind` (`invalid`, `integrity`
or `environment`, the same split as the CLI's exit codes) instead of a dropped
connection, and an unproven run comes back as `unverified` rather than as an
error: an agent must not be able to read a missing proof as a tool problem.

`flot_run` and `flot_verify` replies carry `result`, the one word to act on
([verdict](flows.md#verdict)): `passed`, `partial` (`only` left steps out),
`unverified`, `failed`, and, from `flot_verify`, `broken` when the evidence
does not authenticate. `broken` is also `isError`, as `flot verify` exits 1
for it; the reply's `verdict` is then only what the altered record says.

## Arguments, ping and cancellation

Arguments are checked against the schemas `tools/list` advertises: a wrong
type, a missing required key, an unknown key or an unknown tool is a JSON-RPC
`-32602` error, and nothing runs. Tool calls run one at a time on a worker, so
`ping` is answered during a long run, and `notifications/cancelled` stops the
call's steps and records the run, with no reply to the cancelled request.
