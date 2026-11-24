---
name: flot
description: Use when the project has a flot.toml and you are about to say a change works, after fixing or building something, or when asked whether checks pass, what failed, or what changed between runs. Runs the flow through the flot MCP tools and reports the verdict its receipts prove.
---

# Prove work with flot

This project's `flot.toml` names the commands that prove its work: each step
records a receipt, and each claim is verified only by a command that ran and
exited 0. Report what the receipts prove, not what you expect.

## Before you say it works

1. Call `flot_run`. Steps whose inputs did not change are reused, so this is
   cheap to repeat; do not pass `force` to double-check a reused step.
2. Report the verdict and every claim that is not verified:
   - `passed`: every step succeeded and every claim was proven.
   - `failed`: a step failed or a claim was refuted. Say which, and why.
   - `unverified`: the work completed, but a claim has no proof. That is not
     a pass; say which claim lacks proof.
3. Never call the work done on `failed` or `unverified`.

## When something fails

- The step's inline output keeps its first and last lines. Read all of it with
  `flot_step_output` (`step`, and `stderr: true` for stderr).
- Fix the code under test. Do not edit the flow's proof commands or the files
  they read to make a claim pass; that voids the evidence. If a check itself
  is wrong, say so and leave the change to the user.
- Run `flot_run` again after the fix.

## Looking back

- `flot_runs` lists recorded runs; `flot_show` summarises one.
- `flot_diff` names the steps whose receipts or status changed between two
  runs (by default, the last two).
- `flot_search` finds text in what earlier runs printed, claimed or noted,
  newest first: use it to find when an error first appeared.
- `flot_verify` re-authenticates a run from the store alone.

`flot_run` executes the flow's commands, which are not sandboxed; the user
approves it. Every other flot tool only reads.
