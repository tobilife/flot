# MCP server

`flot mcp` speaks newline-delimited JSON-RPC on stdio, with the same functions
behind it as the CLI, so a host and a human get the same receipts, verdicts and
digests:

## Register flot with your agent

flot must be on your `PATH` (`cargo install` puts it in `~/.cargo/bin`). Run
the registration once; the agent starts `flot mcp` itself, in the directory of
the session, so flot finds the nearest `flot.toml` from there.

```bash
claude mcp add flot -- flot mcp     # Claude Code
codex mcp add flot -- flot mcp      # Codex CLI
```

Any other client that speaks MCP over stdio can run `flot mcp` the same way.

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
