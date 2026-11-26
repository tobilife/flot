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

- **Steps are not sandboxed by default.** A step runs with your user's
  permissions and can do anything you can, including editing the `.flot`
  store. Running a flow you have not read is the same as running a script you
  have not read. A step with a `sandbox` is confined on Linux and macOS (no
  network, writes only beneath named paths), but still reads what you can.
- On macOS, a process a step starts that leaves its process group
  (`setsid`, a daemon) keeps running after the step; flot reports the step's
  output as possibly incomplete but does not stop that process. On Linux
  flot stops it and leaves the step's claims unverified, unless it joined
  flot's own process group. (On Windows the step's job object cannot be
  left.)
- **The guard is not a boundary.** `flot guard` and the Claude Code plugin's
  hook refuse a change an agent names, before it happens, to what proves the
  work or decides whether it is checked
  ([refusing the edit](docs/flows.md#refusing-the-edit)). They read only what
  a call names: a write through a script, a path in a variable, names
  another program produces, or code an interpreter runs (`python -c`) is not
  seen. Only Claude Code runs the hook; another agent, a person and a step's
  own command are not stopped. An agent runs as your user, so it can read
  the signing key and this host's secret, and edit the `.flot` store in any
  way the hook does not see; what it then signs or seals is
  indistinguishable from this host's own. `proofs_from` holds a proof whatever happened to it: one
  that differs from the revision proves nothing.
- On Windows, flot does not check who can read the signing key; it relies on
  the permissions of the user profile it lives in.
- What agents you run as steps send to their providers.
