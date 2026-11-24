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

## Approvals

Every tool except `flot_run` only reads the store, and says so: its MCP
annotations mark it read-only, non-destructive and closed-world, so a host can
let it through without asking. `flot_run` executes the flow's commands, which
are not sandboxed, and is annotated as destructive and open-world; a host
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
| `flot_plan` | predicted execution, reuse and evidence gaps; runs nothing |
| `flot_run` | execute and record receipts (`only`, `force`, `jobs`, `max_cost_usd`) |
| `flot_show` | steps, claim verdicts and limitations of a run |
| `flot_verify` | re-authenticate a run from the store |
| `flot_diff` | compare two runs by receipt identity |
| `flot_receipt` | one receipt, by id or hex prefix |
| `flot_read_object` | complete step output by digest, with `max_bytes` |
| `flot_runs` | recorded runs with verdicts and roots, and any that cannot be read |

## Replies and errors

Every reply carries a text rendering *and* `structuredContent`. A tool failure
comes back as `isError` with the reason and its `kind` (`invalid`, `integrity`
or `environment`, the same split as the CLI's exit codes) instead of a dropped
connection, and an unproven run comes back as `unverified` rather than as an
error: an agent must not be able to read a missing proof as a tool problem.

## Arguments, ping and cancellation

Arguments are checked against the schemas `tools/list` advertises: a wrong
type, a missing required key, an unknown key or an unknown tool is a JSON-RPC
`-32602` error, and nothing runs. Tool calls run one at a time on a worker, so
`ping` is answered during a long run, and `notifications/cancelled` stops the
call's steps and records the run, with no reply to the cancelled request.
