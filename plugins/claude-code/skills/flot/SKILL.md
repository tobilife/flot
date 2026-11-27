---
name: flot
description: Use when the project has a flot.toml and you are about to say a change works, after fixing or building something, or when asked whether checks pass, what failed, or what changed between runs. Runs the flow through the flot MCP tools and reports the verdict its receipts prove.
---

# Prove work with flot

This project's `flot.toml` names the commands that prove its work: each step
records a receipt, and each claim is verified only by a command that ran and
exited 0. Report what the receipts prove, not what you expect.

## Before you say it works

1. Call `flot_plan`. It runs nothing and shows which steps would execute. If
   one that would execute calls an agent or another paid service, say so
   first. Never pass `force` or `max_cost_usd` unless the user asks: a forced
   step runs again, and paid steps cost money each time.
2. Call `flot_run` without `only`. A partial run says nothing about the steps
   it skipped.
3. Report its `result`, every claim that is not verified, every limitation
   and every entry under `refusals`:
   - `passed`: every step of the flow succeeded and every claim was proven.
   - `partial`: the selected steps passed; the others did not run.
   - `unverified`: a claim has no proof, or a proof differs from
     `proofs_from`. Say which.
   - `failed`: a step failed or a claim was refuted. Say which, and why.
4. For each step say whether it ran now or reused an earlier identical
   receipt (`cached`). One with `vouched_by` was proven on another host: name
   its signer only when its `vouched` entry is `confirmed` with a
   `trusted_as` name; otherwise say the signer is not confirmed or not
   trusted here. If you changed a file a step depends on and that step was
   still `cached`, its `inputs` do not name the file: say so, and do not
   count that reuse as proof of your change.
5. Never call the work done unless the result is `passed`.

## When the hook refuses

Stop, and tell the user the path and the reason as given. Do not reach the
file another way: no other tool, shell command, interpreter, script, git
command, MCP file tool or subagent. If a check itself is wrong, say so and
leave the change to the user. The refusal is recorded in the next run.

## When something fails

- The step's inline output keeps its first and last lines. Read all of it
  with `flot_step_output` (`step`, and `stderr: true` for stderr).
- A refuted claim's evidence is what its proof printed, not the step's
  output: read it with `flot_step_output` (`step`, `proof: true`).
- A claim left unverified because the proof fails with the step's `held`
  files as the revision has them means the change passes only tests it
  changed: read that run with `flot_step_output` (`step`, `held: true`) and
  fix the code until the original tests pass too. Change a held test only
  when its old expectation is wrong, and say so.
- A claim left unverified because a mutant survived means the tests pass
  whatever that code does: `flot_show` names each survivor. Write a test
  that fails with the mutation (read the tester's output with
  `flot_step_output`, `step`, `mutants: true`). Do not add the survivor to
  the allow list: only a person may decide a mutant is harmless.
- Fix the code under test, not the flow's proof commands, the files they
  read, or what runs the checks (CI workflows, git hooks, Claude Code
  settings): that voids the evidence.
- A step that did not run says why (`not run: dependency lint (failed) did
  not hold`): report the dependency that failed, not the skipped step.
- Run `flot_run` again after the fix.
- If a run passes after one failed and you changed nothing, the failure was
  not fixed: `flot_diff` between the two says `same key, different result`
  for that step. Report the step as flaky, with what differed, and run
  `flot flaky` if the user wants the history. Never call it fixed.

## Never without being asked

`flot trust`, `flot key`, `flot sign`, `flot import`, `flot pull`,
`flot push`, `flot gc`, `flot init --force`, and the flags `--force`,
`--max-cost` and `--proofs-from`. They decide whose word counts, discard
evidence, move the bar the work is held to, or spend money.

## Without the MCP tools

Run `flot plan`, then `flot run --json`. The exit code is the result: 0
passed, 1 failed or evidence that does not authenticate, 2 unverified or
partial, 3 invalid request, 4 something outside the flow failed (git, a
revision, the sandbox). In `flot verify`, `authorship: this host's key`
(`this_host`) only says this machine signed the run: a label, not trust.

## Looking back

`flot_runs` lists recorded runs, `flot_show` summarises one, `flot_diff`
names the steps that changed between two (by default the last two),
`flot_search` finds what earlier runs printed, claimed or noted, and
`flot_verify` re-authenticates a run from the store alone: read its `result`.

`flot_run` executes the flow's commands, unsandboxed unless a step asks to
be; the user approves it. Every other flot tool only reads.
