# Architecture

```
core      digest · receipt · flow · metrics · compare
          attest · fault · secrets · shell · powershell  pure model, no IO
store     content-addressed objects, receipts, runs
engine    keying · claims · scheduler · exec · runner   flow -> receipts
          git · baseline · sandbox
service   Workspace: plan run show verify diff receipt
          read_object runs gc sign bundle import key    the one contract
          trust search push pull guard doctor
adapters  cli · mcp                                     parse and render only
main.rs   composition root: hands the MCP server to the CLI
```

Dependencies only point down, adapters never import each other, and `core`
performs no IO, so identity derivation cannot depend on the machine it runs on.
`tests/architecture.rs` enforces all three on every test run; each rule was
checked by injecting a violation and watching it fail.

Inside the engine, policy and mechanism are split. `keying` decides a step's
identity, `claims` decides what a proof established (pure: the only path to
`verified` is readable in one function), `scheduler` decides when a step may
start and knows nothing about steps, and `runner` only sequences them around
process execution. `baseline` compares a proof with a git revision through
`git`, which also carries runs to and from remotes. `exec` runs each command
in a process group of its own; on Linux flot is also a child subreaper and
tags each command with `FLOT_COMMAND_ID`, so after a command it can find in
`/proc` every adopted process that left its group, stop it, and hand it to
the runner, which turns it into a limitation and unverified claims.

One rule, `service::audit::Outcome`, decides what a run comes to (`broken`,
`failed`, `unverified`, `partial`, `passed`, the worst that holds); the CLI's
exit codes and the MCP server's `result` and `isError` both read it.

The guard is split the same way. `core::shell` and `core::powershell` read a
command line into the words it may write, without touching the disk; the
CLI adapter expands those words against the disk and reads hook payloads
and tool inputs; `service::Guard` judges a path against every flow above
it, through links and by what a directory holds, and asks git what a
whole-tree git command would change.

## Current scope

Working today: flow parsing and validation, dependency ordering, step keys over
input content, receipt sealing and authentication, the claim gate, admission and
delivery budgets, bounded-concurrency scheduling, cancellation and interruption,
the content store with host-sealed cache and trust, the CLI with typed exit
codes, an MCP stdio server over the same functions, authenticated receipt
bundles, spend accounting with a run cost ceiling, signed attestations with a
local trust list and key handovers, guarded proofs, produced files kept and restored, an enforced
layer architecture, attempts with feedback, candidates, run search, sharing through git that refuses credential-shaped tokens, a Claude Code plugin whose guard reads every tool call (edits, shell and PowerShell commands, and the paths other tools name), `flot doctor`, and 419 tests including end-to-end runs through the real
binary, a real MCP handshake and a cross-store bundle import.

Not there yet: confining steps on Windows, and confining reads (a step
with a [sandbox](flows.md#sandbox) still reads what you can).

Steps run in `sh` everywhere. On Linux and macOS a step leads its own process
group and is stopped with signals (on Linux, with what left the group); on Windows, where Git for Windows provides
`sh`, a job object holds the step and everything it starts, and the store lock
uses `LockFileEx`. CI runs the suite on all three.
