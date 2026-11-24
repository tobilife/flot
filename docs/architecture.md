# Architecture

```
core      digest · receipt · flow · metrics · compare
          attest · fault                                 pure model, no IO
store     content-addressed objects, receipts, runs
engine    keying · claims · scheduler · exec · runner   flow -> receipts
          git · baseline · sandbox
service   Workspace: plan run show verify diff receipt
          read_object runs gc sign bundle import key    the one contract
          trust search push pull guard
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
`git`, which also carries runs to and from remotes.

## Current scope

Working today: flow parsing and validation, dependency ordering, step keys over
input content, receipt sealing and authentication, the claim gate, admission and
delivery budgets, bounded-concurrency scheduling, cancellation and interruption,
the content store with host-sealed cache and trust, the CLI with typed exit
codes, an MCP stdio server over the same functions, authenticated receipt
bundles, spend accounting with a run cost ceiling, signed attestations with a
local trust list and key handovers, guarded proofs, produced files kept and restored, an enforced
layer architecture, attempts with feedback, candidates, run search, sharing through git, a Claude Code plugin, and 322 tests including end-to-end runs through the real
binary, a real MCP handshake and a cross-store bundle import.

Not there yet: confining steps outside Linux, and confining reads (a step
with a [sandbox](flows.md#sandbox) still reads what you can).

Steps run in `sh` everywhere. On Linux and macOS a step leads its own process
group and is stopped with signals; on Windows, where Git for Windows provides
`sh`, a job object holds the step and everything it starts, and the store lock
uses `LockFileEx`. CI runs the suite on all three.
