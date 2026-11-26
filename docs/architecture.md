# Architecture

```
core      digest · receipt · preview · flow · metrics · compare
          attest · fault · secrets · shell · powershell  pure model, no IO
store     content-addressed objects, receipts, runs
engine    keying · claims · scheduler · exec · runner   flow -> receipts
          git · baseline · sandbox · progress
service   Workspace: plan run show verify diff receipt
          read_object runs gc sign bundle import key    the one contract
          trust search push pull guard doctor
adapters  cli · mcp                                     parse and render only
main.rs   composition root: hands the MCP server to the CLI
```

Dependencies only point down, adapters never import each other or name the
command-line readers (`core::shell`, `core::powershell`), and `core` performs
no IO, so identity derivation cannot depend on the machine it runs on.
`tests/architecture.rs` enforces all four on every test run; each rule was
checked by injecting a violation and watching it fail. An adapter is the
file or directory right below `adapters/`, so `adapters/cli/args.rs` is part
of `cli`; the test runs its rules on a scratch tree to show it tells them
apart. Literal goldens pin the bytes of step keys, receipts, run manifests
(digest and signature), bundles and cache entries, so moving code that
changes any of them fails a test.

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
`progress` is what a run reports while it goes (steps starting, the lines
`exec` reads from them, how each ended) to a sink in `RunOptions`; the
engine only calls it, the CLI renders it on stderr, and nothing it carries
reaches a receipt.

One rule, `service::audit::Outcome`, decides what a run comes to (`broken`,
`failed`, `unverified`, `partial`, `passed`, the worst that holds); the CLI's
exit codes and the MCP server's `result` and `isError` both read it.

The guard is split the same way. `core::shell` and `core::powershell` read a
command line into the words it may write, without touching the disk.
`service::guard` does the rest: it expands those words against the disk and
the environment, reads the scripts a command runs, judges each path against
every flow above it, through links and by what a directory holds, asks git
what a whole-tree git command would change, refuses a call it cannot judge
where a flow is, and notes each refusal for the next run. The CLI adapter
reads a hook payload, a transcript or its own arguments into a call and
renders the answer.

## Current scope

Working today: flow parsing and validation, dependency ordering, step keys over
input content, receipt sealing and authentication, the claim gate, admission and
delivery budgets, bounded-concurrency scheduling, cancellation and interruption,
the content store with host-sealed cache and trust, the CLI with typed exit
codes and live progress on stderr, an MCP stdio server over the same
functions, authenticated receipt bundles, spend accounting with a run cost ceiling, signed attestations with a
local trust list and key handovers, guarded proofs, produced files kept and restored, an enforced
layer architecture, attempts with feedback, candidates, run search, sharing through git that refuses credential-shaped tokens, a Claude Code plugin whose guard reads every tool call (edits, shell and PowerShell commands, and the paths other tools name), `flot doctor`, and 449 tests including end-to-end runs through the real
binary, a real MCP handshake and a cross-store bundle import.

Not there yet: confining steps on Windows, and confining reads (a step
with a [sandbox](flows.md#sandbox) still reads what you can).

Steps run in `sh` everywhere. On Linux and macOS a step leads its own process
group and is stopped with signals (on Linux, with what left the group); on Windows, where Git for Windows provides
`sh`, a job object holds the step and everything it starts, and the store lock
uses `LockFileEx`. CI runs the suite on all three.
