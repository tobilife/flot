# Writing flows

A flow is a TOML file, `flot.toml` by default. A step declares what it runs, what it reads, what it asserts,
and how that assertion is proved. A key flot does not know is an error, not
ignored: a misspelled `verify` or `inputs` would otherwise silently weaken a
gate.

```toml
[flow]
name = "release-candidate"
budget = { timeout_secs = 900, max_inline_bytes = 4096 }

[[step]]
id = "implement"
run = "gjc -p 'implement .flot-work/spec.md'"
needs = ["spec"]
inputs = [".flot-work/spec.md"]
outputs = ["src/**/*.rs"]
claims = ["the workspace still compiles after the agent's edits"]
verify = "cargo build --all-targets --quiet"
```

## Step keys

 Digest over the command, the declared environment, the content of
every declared input file and proof input, the verify command, the declared
output patterns, and the receipt ids of upstream steps. Equal key ⇒ the same work over the same facts ⇒ the receipt is reused.
Declaring inputs is what makes a cache hit trustworthy; a step with none carries
a limitation saying so, in `flot plan` and in its receipt.

## Receipts

 Split in two. `identity` is deterministic — command, step key,
status, exit code, output digests, inputs, produced files, dependency receipts, claim verdicts,
limitations, deriving tool. The receipt id *is* the digest of that body, so an
edited receipt stops authenticating. `observed` carries timing, host and bounded
inline previews, and is never hashed — two honest runs disagree on milliseconds,
not on facts.

## Claims

 Three states: `verified` (a proof command ran after the step
succeeded and exited zero), `refuted` (it ran and rejected), `unverified`
(anything else — no proof command, step failed, proof timed out or was killed
by a signal, or a file the proof judges changed while it ran). Each one
records the proof command, its exit code and a digest of its output.
`allow_failure = true` lets a run continue past a failed step, but proves
nothing: that step's claims stay unverified.

## Verdict

 `failed` if any step did not reach `ok` or any claim was refuted;
`unverified` if any claim lacks proof; `passed` only when every step succeeded
and every claim is proven.

## Budgets

 `timeout_secs` and `max_output_bytes` (256 MiB per stream by
default) are admission limits: they stop work and report it, never as a
success. `max_inline_bytes` is a delivery budget: it trims what travels
inline, and the complete stream stays retrievable with `flot cat <digest>`.

## Concurrency

 `flot run --jobs N`, or `max_parallel = N` in `[flow]`, runs up
to N independent steps at once; a step still waits for its dependencies. Because
completion order never enters a receipt or the run root, `--jobs 8` and
`--jobs 1` produce byte-identical digests for the same work — verified by test.
Concurrency is opt-in and the manifest records it as a limitation: flot checks
the files steps *declare* as outputs (see [Outputs](#outputs)), but cannot see files they
write without declaring them. A failing branch cancels only its own dependents; an
independent branch still completes.

Measured on a fan-out of three independent one-second steps joined by a gate:

```
flot run             3097ms   root sha256:d7b7703db719...
flot run --jobs 3    1066ms   root sha256:d7b7703db719...
```

The 2.9x is bounded by the three sleeps, and the root is unchanged because
wall-clock never enters a receipt.

## Execution

 `run` and `verify` go through `sh -e -c` from the flow root, so
the first failing line of a multi-line command fails the step; without `-e` an
agent call that failed, followed by a line that succeeded, would be recorded as
success. Write `|| true` where a failure is expected. A step is one unit of
work: anything it left running in its process group when it exits is stopped,
and reported. A process that escapes the group (`setsid`, a daemon) cannot hold
the run open either; flot stops reading its output after a short grace period
and says the captured output may be incomplete, but it keeps running. Files the
proof judges are re-read after the proof, so such a process cannot change them
unnoticed. Ctrl-C (or SIGTERM, SIGHUP) stops the running steps with their
process groups and still records the run; a second one exits at once.

Steps are not sandboxed. A step runs with your user's permissions and
environment and can read or write anything you can, the `.flot` store included.
flot records and judges what steps did; it does not confine them. Run flows
you have read.

A step that cannot run at all, say because its command cannot start, gets a
`failed` receipt saying why, and the run still closes with a manifest that
records what earlier steps spent. Only a store that cannot record receipts
stops a run without one.

## Cancellation

 A step whose dependency failed, or succeeded with claims a
proof refuted, is never started; it gets a `cancelled` receipt naming the
dependency and why. Refuted work already fails the run, so building on it could
only spend time and money. An *unproven* dependency does not block: its claims
are unknown, not false.

## Guarded proofs

 `verify_inputs` names the files a proof reads, such as an
acceptance test. Their content is part of the step key, so tightening the test
re-proves the claim instead of reusing a verdict the new test never gave. They
are also re-read after the step runs: a step that edited its own proof gets
unverified claims and the proof is not run. A step that edits its declared
`inputs` is reported too, since its receipt then describes files that no longer
exist.

## Outputs

 `outputs` names the files a step produces. When the step finishes
their content goes into the store and the receipt, and a success that produced
no file for a declared pattern is `failed`: it did not do what the flow says.
Reusing a receipt puts missing outputs back, so the files on disk are the ones
its claims were proven against. An output that exists with different content
is never overwritten, and never vouched for by a receipt that proved other
content: the receipt is not reused and the step runs again, so its proof judges
the file that is actually there. Output files travel in a bundle, so an
importing host gets the produced artifact, not just a report about it. Two steps that
produce the same file without one needing the other are reported, because which
content survives depends on scheduling. Patterns must stay inside the flow root,
a step cannot declare its own proof as an output, and a receipt is never reused
if it names a file the local flow does not declare for that step. A file reached
through a symbolic link is neither recorded as an output nor restored through:
the link could point anywhere. Outputs are what the patterns match
when the step finishes, so a stale file left by an earlier run is recorded too;
keep patterns as specific as the step's real products.
