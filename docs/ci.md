# In CI

flot runs inside CI the way it runs in a terminal. What CI adds is a reader
the agent does not control: a job that checks the run against the base
branch, reports it on the pull request, and can be required before a merge.

## The GitHub action

This repository is also a GitHub action. It installs a release binary
(checked against its `.sha256`), runs the flow, checks the run with
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
      - uses: tobilife/flot@v0.2.2
```

The report goes to the job summary and, on a pull request, into one comment
the action updates on later runs. The job fails unless `flot verify` exits 0:
the run passed, its evidence is intact, and its proofs are as on the base
branch. Make the job a required status check in the branch's protection rules
and a pull request cannot merge on a claim that a weakened check proved.

| Input | Default | |
|---|---|---|
| `version` | the release of the tag you use | the flot release to install; `path` uses a flot already on `PATH` |
| `working-directory` | `.` | where to look for `flot.toml` |
| `run` | `true` | run the flow first; `false` checks the latest run an earlier step executed |
| `proofs-from` | the base branch on a pull request | a revision every proof must match; `none` turns it off |
| `signers` | | a file of trusted signers, see below |
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

## Reusing a developer's signed run

A run the developer signed and shared with `flot push --sign` need not be
executed again in CI. With `signers: .github/flot-signers`, the action reads
that file (`NAME KEY` per line, as `flot trust add` takes them; `#` starts a
comment) **from the base branch**, trusts those keys, and runs
`flot pull --require-trusted` before the flow. Steps whose key matches a
trusted receipt are then reused; anything else runs in CI. The file is read
from the base branch because a pull request that could add its own key could
vouch for itself; protect the file on that branch too, for example with a
`CODEOWNERS` entry.

By hand, in a clone with a fresh host key (real output):

```text
$ git show origin/main:.github/flot-signers | while read -r name key; do flot trust add "$name" "$key"; done
trusting ed25519:e9a1cf6df83179e7e7bee570fbc6007e3cb9b171e72808988efd902dfbb7e446 as dev
$ flot pull --require-trusted
imported run 1795532150429-2b2b51cba86d  root 2b2b51cba86d  trusted  reusable
origin of flow ci: 1 imported, 0 already here, 0 refused
$ flot run --proofs-from origin/main
cached   t                    ok        12ms  claims 1v/0u/0r  receipt 5c8d316a79ba
```

`flot verify` checks runs executed on the host it runs on, so CI runs the flow
after the pull, and that run is what it verifies; a pulled run alone is named
by its id.

## Limits

- A step runs CI's own commands on the pull request's code, as any CI job
  does; flot does not sandbox it.
- The comment needs `pull-requests: write`. A pull request from a fork gets a
  read-only token, so the action leaves a warning and the report stays in the
  job summary.
- The action is tested in this repository's CI on pushes to `main`; the
  comment path runs only on a pull request.
