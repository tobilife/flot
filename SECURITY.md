# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through
[GitHub private vulnerability reporting](https://github.com/tobilife/flot/security/advisories/new).
Include the flot version (`flot --version`), your platform, the steps to
reproduce and what an attacker gains. Please do not open a public issue.

Fixes land on `main`; there are no maintained release branches.

## What counts as a vulnerability

flot's security promises are about evidence. Examples of what we treat as
vulnerabilities:

- a claim reported `verified`, or a run reported `passed`, without a proof that
  ran and exited zero;
- a receipt, run, object or bundle that was altered but still authenticates, or
  that `flot verify` or `flot import` accepts;
- a signature from an untrusted or revoked signer that is reported as trusted,
  or that lets an imported receipt stand in for local work;
- a cache entry or trust list from another host or another store that flot
  believes;
- a bundle or store record that makes flot read or write outside the flow root
  or the `.flot` store.

## Out of scope

- **Steps are not sandboxed.** A step runs with your user's permissions and can
  do anything you can, including editing the `.flot` store. Running a flow you
  have not read is the same as running a script you have not read.
- On Linux and macOS, a process a step starts that leaves its process group
  (`setsid`, a daemon) keeps running after the step; flot reports the step's
  output as possibly incomplete but does not stop that process. (On Windows the
  step's job object cannot be left.)
- On Windows, flot does not check who can read the signing key; it relies on
  the permissions of the user profile it lives in.
- What agents you run as steps send to their providers.
