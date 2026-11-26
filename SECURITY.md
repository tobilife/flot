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
- A process a step starts that leaves its process group (`setsid`, a
  daemon) is stopped on Linux and macOS, and the step's claims stay
  unverified, with two exceptions: one that joined flot's own process group,
  and, on macOS only, one that cleared its environment and left its parent
  within about 20 ms, before flot looked at the processes below the step.
  Such a process keeps running after the step; flot reports the step's
  output as possibly incomplete if it still holds it. A process started on
  the step's behalf by something outside it (`systemd-run`, `launchctl`,
  `open`, a server already running) is not the step's and is not stopped.
  (On Windows the step's job object cannot be left.)
- **The guard is not a boundary.** `flot guard` and the Claude Code plugin's
  hook refuse a change an agent names, before it happens, to what proves the
  work or decides whether it is checked
  ([refusing the edit](docs/flows.md#refusing-the-edit)). They read
  the scripts and interpreter code a call runs and refuse code built when it
  runs, but they read text: a name assembled from parts, a path a program
  prints, a script copied before it runs and compiled programs get past
  them. The hook refuses any call that names the signing key, a retired copy
  or this host's secret, reads included; `flot key show` stays allowed. Only
  Claude Code runs the hook; another agent, a person and a step's own
  command are not stopped. An agent runs as your user, so outside the hook
  it can still read the signing key and this host's secret, and edit the
  `.flot` store; what it then signs or seals is indistinguishable from this
  host's own. Keep the key outside agent projects. `proofs_from` holds a proof whatever happened to it: one
  that differs from the revision proves nothing.
- On Windows, flot does not check who can read the signing key; it relies on
  the permissions of the user profile it lives in.
- What agents you run as steps send to their providers.
