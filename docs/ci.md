# In CI

flot runs inside CI the way it runs in a terminal. What CI adds is a reader
the agent does not control: a job that checks the run against the base
branch, reports it on the pull request, and can be required before a merge.

## The GitHub action

This repository is also a GitHub action. It installs a release binary,
checked against its `.sha256` and against its build provenance attestation
(`gh attestation verify`, which must find it signed by this repository's
release workflow for the version's tag), runs the flow, checks the run with
`flot verify --markdown` and, on a pull request, holds every proof to the
base branch:

```yaml
on: pull_request

jobs:
  flot:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      pull-requests: write   # for the comment
    steps:
      - uses: actions/checkout@v4
      - uses: tobilife/flot@v0.2.10
```

The report goes to the job summary and, on a pull request, into one comment
the action updates on later runs. The job fails unless `flot verify` exits 0:
the run passed, its evidence is intact, and its proofs are as on the base
branch. Make the job a required status check in the branch's protection rules
and a pull request cannot merge on a claim that a weakened check proved.

| Input | Default | |
|---|---|---|
| `version` | the release of the tag you use | the flot release to install, 0.2.4 or later (earlier releases have no attestation); `path` uses a flot already on `PATH` |
| `working-directory` | `.` | where to look for `flot.toml` |
| `run` | `true` | run the flow first; `false` checks the latest run an earlier step executed |
| `proofs-from` | the base branch on a pull request | a revision every proof must match; `none` turns it off |
| `comment` | `true` | comment on the pull request |
| `token` | `github.token` | token for the comment |

Outputs: `verdict` (the recorded verdict) and `exit-code` (of `flot verify`).

This repository's own CI runs its gate through the action (the `action` job in
[ci.yml](../.github/workflows/ci.yml)), with `version: path` and a flot built
from the commit under test.

## The report

`flot verify --markdown` prints the check as Markdown; its headline is the
same judgment as the exit code. Claims, limitations and reasons are written
as text: brackets, pipes, `<` and the like are escaped, so an agent's claim
cannot add a link, an image or a table row to the comment. From a real run:

```text
$ flot verify --markdown --proofs-from origin/main
### flot: passed

Run `1795532156970-2b2b51cba86d` of flow ci, root `2b2b51cba86d`; recorded verdict passed.

- Integrity: intact
- Proofs: as at origin/main \(592c8bd3fa70\)
- Authorship: unsigned
- Claims: 1 verified, 0 unverified, 0 refuted

| Step | Status | Claim | |
|---|---|---|---|
| `t` | ok, reused | o | verified |
```

(the limitations that follow, in a collapsed `<details>` block, are left out
here.)

When files changed since the revision that no step read, the report adds a
line `Changes no step read:` naming them (up to 20), so a reviewer sees what
the claims do not cover ([changes no step read](flows.md#changes-no-step-read)).

## Reusing a developer's signed run

A run the developer signed and shared with `flot push --sign` need not be
executed again in CI. Before the action, a step can trust the developers'
keys and pull their runs; steps whose key matches a trusted receipt are then
reused, and anything else runs in CI. Read the keys **from the base branch**
(here a file of `NAME KEY` lines, as `flot trust add` takes them): a pull
request that could add its own key could vouch for itself. Protect the file
on that branch too, for example with a `CODEOWNERS` entry. The signing key
never reaches CI: the developer signs on their own host, and CI only checks
the signature against the public key the base branch names.

This repository does so: its [`action` job](../.github/workflows/ci.yml)
reads [`.github/flot-signers`](../.github/flot-signers) from the base branch
(from the commit itself on a push to `main`), trusts those keys, runs
`flot pull --require-trusted`, and then the action. Where the base branch
has no such file, every step runs in CI. The flow's `toolchain` step runs
every time (`always = true`, see [step keys](flows.md#step-keys)), so CI
reuses the developer's other steps only while its `rustc -vV` and `cargo -V`
print what the developer's did. This repository pins its toolchain in
[`rust-toolchain.toml`](../rust-toolchain.toml), which rustup follows here and
which the workflows read to install the same one, so a run signed with the
pinned toolchain is reused in CI; a different `rustc` runs every step again.
The other jobs still build and test every commit themselves.

After the local run, the developer shares it with `flot push --sign`. A
fresh clone with a fresh store, doing what that job does (real output, on a
Linux file system, against a run signed on the WSL `/mnt` drive and pushed to
a local remote):

```text
$ git show HEAD:.github/flot-signers | while read -r name key; do case "$name" in ''|'#'*) continue;; esac; flot trust add "$name" "$key"; done
trusting ed25519:3a9f3136fc4fc8b7d3d96d45cf987653063fbf0ca394dcc75cac2424da9097c9 as tobilife
$ flot pull --require-trusted
imported run 1795577477469-c44b96937747  root c44b96937747  trusted  reusable
store of flow flot-self-check: 1 imported, 0 already here, 0 refused
$ flot run
cached   format               ok        243ms  claims 1v/0u/0r  receipt c806d40b7569
cached   build                ok        5564ms  claims 1v/0u/0r  receipt bfa4474e4b2d
cached   lint                 ok        725ms  claims 1v/0u/0r  receipt b97c49bccc51
cached   test                 ok        5815ms  claims 2v/0u/0r  receipt 14a2d4f2eb4a

PASSED 1795577509581-c44b96937747  root c44b96937747  77ms
limitations:
  - proofs checked against origin/main (a0dbb7f393be)
```

The root is the signed run's. The first attempt reused nothing: on the
`/mnt` drive every file reads as executable, so every input was keyed as
executable there and as not executable in the clone. Inputs git tracks are
now keyed by the bit git records where git does not trust the file system
([step keys](flows.md#step-keys)).

CI does the same on `main`. In run
[45550616858](https://github.com/tobilife/flot/actions/runs/45550616858), for
the 0.2.5 release commit, the `action` job printed (key shortened here):

```text
trusting ed25519:3a9f…97c9 as tobilife
imported run 1795534848392-65ee8029fb2c  root 65ee8029fb2c  trusted  reusable
imported run 1795577803940-a32a34000eec  root a32a34000eec  trusted  reusable
imported run 1795582620687-f43861278255  root f43861278255  trusted  reusable
origin of flow flot-self-check: 3 imported, 0 already here, 0 refused
cached   format               ok        242ms  claims 1v/0u/0r  receipt 86de9db3ae30  vouched for by tobilife
cached   build                ok        7184ms  claims 1v/0u/0r  receipt 522282d58cd5  vouched for by tobilife
cached   lint                 ok        3321ms  claims 1v/0u/0r  receipt c8a8d5c3299c  vouched for by tobilife
cached   test                 ok        5955ms  claims 2v/0u/0r  receipt 728a01c3b463  vouched for by tobilife

PASSED 1795582863394-f43861278255  root f43861278255  64ms
limitations:
  - proofs checked against origin/main (ea2e1bd60799)
```

The root is the signed run's, and each step names who vouched for it, by the
name the trust list gives the signer. The job writes `flot verify --markdown`
to the job summary rather than the log; the same commands in a fresh clone of
that commit, with flot 0.2.5 from crates.io, print:

```text
$ flot verify --markdown
### flot: passed

Run `1795589241230-f43861278255` of flow flot\-self\-check, root `f43861278255`; recorded verdict passed.

- Integrity: intact
- Proofs: as at origin/main \(ea2e1bd60799\)
- Authorship: unsigned
- Reused from signed runs: 4 step(s), vouched for by tobilife
- Claims: 5 verified, 0 unverified, 0 refuted

| Step | Status | Claim | |
|---|---|---|---|
| `format` | ok, reused, vouched for by tobilife | every source file is rustfmt\-clean | verified |
| `build` | ok, reused, vouched for by tobilife | the library, binary and test targets compile | verified |
| `lint` | ok, reused, vouched for by tobilife | clippy reports no warnings on any target | verified |
| `test` | ok, reused, vouched for by tobilife | the unit suite passes | verified |
|  |  | the end\-to\-end CLI suite passes | verified |

<details><summary>Limitations</summary>

- proofs checked against origin/main \(ea2e1bd60799\)

</details>
```

CI's own run is unsigned, so its report reads `Authorship: unsigned`, and
nothing covers the signer keys its steps record. A signer is therefore named
only when the store also holds a run the signer signed that kept the same
receipt for the step, here the pulled run, and only while the trust list
still holds the key. Otherwise the key is shown and marked, as in
`vouched for by ed25519:a4929b56a758611c (not trusted here)` once the signer
was removed, or `vouched for by ed25519:343ff0b6dc243ffa (not confirmed here)`
for a run that names a signer whose signature this store does not hold.

`flot verify` checks runs executed on the host it runs on, so CI runs the flow
after the pull, and that run is what it verifies; a pulled run alone is named
by its id.

## Limits

- A step runs CI's own commands on the pull request's code, as any CI job
  does; flot confines it only if the step has a `sandbox`.
- The comment needs `pull-requests: write`. A pull request from a fork gets a
  read-only token, so the action leaves a warning and the report stays in the
  job summary.
- The action runs in this repository's CI on every push and pull request.
  It was tried on a test pull request
  ([#1](https://github.com/tobilife/flot/pull/1), closed): the first commit
  left the proofs as on `main`, and the comment said `passed`; the second
  changed the `test` step's `verify` to `true`, and the same comment was
  updated to "unverified", naming `test: its verify command differs`, and the
  job failed.
