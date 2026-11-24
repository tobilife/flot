# Running coding agents

A step can run any agent CLI as a process; flot judges what it did with a
proof command the agent did not write, and records what it spent.

## A real run, end to end

`examples/slugify` is a flow that has been run end to end against Claude Code
2.1.330: one agent implements `SPEC.md`, `check_slug.py` (which no agent wrote)
proves it, a second agent reviews. The runs below used the example as it was
then; it has since been hardened so that each agent step removes its artifact
before calling the agent and reports spend even when the agent fails. What
happened, in order:

```
1. first spec              implement 1v, review 1v (APPROVE)    PASSED  $1.94
2. re-run, no changes      both cached                          PASSED  $0.00  reused $1.94
3. check adds "Søren"      both cached -- the bug, see below    PASSED  $0.00
   -- verify_inputs added --
4. same check, fresh store implement refuted, review REJECT     FAILED  $1.84
   -- refuted dependencies now block --
5. re-run                  implement refuted, review skipped    FAILED  $0.64
6. spec clarified (ø, ß)   implement 1v, review 1v (APPROVE)    PASSED  $1.67
7. re-run, no changes      both cached                          PASSED  $0.00  reused $1.67
```

Run 3 is the bug this exercise found: the check now failed on the agent's
`slug.py` (8/9), yet flot reused the old receipt and printed `PASSED`, because
the file the proof reads was not part of the step key. `verify_inputs` fixed
it (run 4). Run 4 also spent $0.83 reviewing work whose claim was already
refuted; run 5 is the same situation after making refuted dependencies block.
In run 1 both agents said they could not run the check themselves (their shell
permission was denied) and reasoned about the code instead; the claim was
settled by flot running the check, not by what the agents wrote.

Token counts include cache reads and writes, which dominate an agent call:
answering "ok" alone billed 60,489 input tokens and $0.78. The `jq` mapping in
the examples sums them; reading only `usage.input_tokens` under-reports by four
orders of magnitude.

After `outputs` landed, one more fresh run of the same example:

```
8. outputs declared        implement 1v, review refuted (REJECT) FAILED  $2.10
```

The acceptance check passed 9/9, and the reviewer rejected anyway, with a real
defect the check does not cover: `slug.py` applies the ø/æ table *before*
stripping accents, so `ǿ`, which decomposes to `ø` plus an accent, vanishes
(`slugify("Ǿre") == "re"`, confirmed by running it). The run is `FAILED`
because a refuted review is a refuted claim. Then `slug.py` was deleted, and
`flot run --only implement` restored it from the receipt byte for byte at no
cost; a bundle imported into an empty directory holding only the flow, spec
and check did the same there. That bundle was unsigned; a later review made
imports reusable only from trusted signers (see [Evidence is not cache](evidence.md#evidence-is-not-cache)), so
today the same hand-off needs `bundle --sign` and `trust add`.

## Three reviewers in parallel

`examples/parallel-review.toml` was run once, copied to the root of a clone of
this repository, against a one-line change of the default step timeout
(900 s to 300 s), with Claude Code 2.1.330 and `--jobs 3`:

```
ran      stage                ok        11ms  claims 1v/0u/0r
ran      contracts            ok        56491ms  claims 1v/0u/0r
ran      performance          ok        11247ms  claims 1v/0u/0r
ran      security             ok        11358ms  claims 1v/0u/0r
ran      gate                 ok        11638ms  claims 2v/0u/0r

PASSED  72526ms
spent $3.252855 (in 1107882 / out 5933 tokens, 3 reporting step(s), 2 silent)  ceiling $4.000000
```

The security and performance reviewers both wrote `NONE`, which is right for
that change. The contract reviewer named the real consequence (a step that ran
for 5 to 15 minutes would now time out and fail the run) and a real gap: the
budget defaults were not documented anywhere. They are now, in
[Budgets](flows.md#budgets).

The claims here only prove that each reviewer wrote a verdict, not that the
verdict is right; the review itself is evidence for a person to read.

Spend was dominated by context, not by the answer: each `NONE` cost about
$0.90, almost all of it in the user's own Claude Code setup (plugins, hooks,
personal instructions and MCP servers) that every call loaded. The example now
runs each agent apart from that setup (`--setting-sources project`,
`--strict-mcp-config`, and only the tools the step needs). The same change,
staged this time, reviewed again:

```
ran      stage                ok        11ms  claims 1v/0u/0r
ran      contracts            ok        79735ms  claims 1v/0u/0r
ran      performance          ok        67966ms  claims 1v/0u/0r
ran      security             ok        72592ms  claims 1v/0u/0r
ran      gate                 ok        11991ms  claims 2v/0u/0r

PASSED  96161ms
spent $0.416301 (in 105481 / out 3401 tokens, 3 reporting step(s), 2 silent)  ceiling $4.000000
```

$3.25 became $0.42: each `NONE` cost $0.04 to $0.05, and the contract
reviewer, which still found the same consequence, $0.33. The second run took
longer in wall-clock time, so the saving is in tokens, not in latency.
Staging the change is why the example now diffs against `HEAD`: `git diff`
alone shows nothing once a change is staged, and the `stage` step's own proof
(`test -s .flot-work/change.diff`) would have stopped the run before any
reviewer was paid for. The two `silent` steps are the shell steps, which report
no metrics because they spend nothing; the run names them.

## Spend and the cost ceiling

flot does not parse any agent's output format. Each executed step gets a path in
`$FLOT_METRICS`; a step that knows what it spent writes

```json
{ "input_tokens": 1200, "output_tokens": 340, "cost_usd": 0.0123 }
```

and flot records it on the receipt as an observation (never in the hashed
identity, since two honest runs can bill differently). The run reports spent
cost, tokens, steps that reported nothing (`silent`: unknown, not zero), and cost
recorded by reused receipts, which this run did not pay again.

`max_cost_usd` in `[flow]`, or `flot run --max-cost USD`, stops *starting* steps
once executed steps have reported that much. Reuse costs nothing, so a cache hit
is never blocked. Steps already running when the ceiling is crossed finish, and
the run states the overshoot:

```
spent $1.200000 (in 200 / out 20 tokens, 2 reporting step(s))  ceiling $1.000000
  - c: not run: run cost ceiling of $1.000000 reached ($1.200000 spent by executed steps)
  - spent $1.200000 against a $1.000000 ceiling: the ceiling is checked before each step starts, so steps already running when it was crossed finished
```

A malformed metrics file is reported and treated as unknown spend; it never
reads as zero.
