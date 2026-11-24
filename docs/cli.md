# CLI reference

| Command | Purpose |
|---|---|
| `flot init` | write a starter `flot.toml` in the current directory |
| `flot plan [--only STEP] [--force] [--proofs-from REV]` | predicted execution, reuse and evidence gaps; runs nothing |
| `flot run [--only STEP] [--force] [--jobs N] [--max-cost USD] [--sign] [--proofs-from REV]` | execute and record receipts; `--sign` then signs the run; `--proofs-from` holds each proof to a git revision ([proofs from a revision](flows.md#proofs-from-a-revision)) |
| `flot sign [RUN]` | sign a run executed here with the host key |
| `flot show [RUN]` | steps, claims and limitations of a run |
| `flot verify [RUN] [--proofs-from REV] [--markdown]` | re-authenticate a run from the store alone; with a revision (or the flow's `proofs_from`), also check its proofs are as they are there; `--markdown` prints the check as a report ([in CI](ci.md)) |
| `flot guard PATH... [--hook]` | name the paths whose change would touch the proofs and exit 2 if any; `--hook` answers a Claude Code `PreToolUse` payload |
| `flot diff [LEFT] [RIGHT]` | step-by-step comparison by receipt identity; by default `latest~1` against `latest` |
| `flot receipt ID` | one receipt as JSON (id prefix accepted) |
| `flot log STEP [RUN] [--stderr] [--candidate NAME] [--attempt N]` | complete output one step of a run printed; by default the receipt the run kept, else another candidate it ran or an earlier attempt |
| `flot cat DIGEST` | one stored object, byte-exact |
| `flot runs [--limit N]` | recorded runs, newest first; imported ones are marked |
| `flot search QUERY [--runs N] [--limit N]` | lines of recorded output, claims and limitations that contain QUERY (literal, any case), newest run first; a reused step is searched in the run that executed it, and attempts and candidates not kept are searched too |
| `flot gc [--dry-run]` | drop evidence no recorded run references |
| `flot bundle [RUN] [-o FILE] [--sign] [--allow-secrets]` | one portable file: run, receipts, every object they name (streams, produced files, proofs), signatures; a run that carries a credential-shaped token is refused |
| `flot import FILE [--require-trusted]` | authenticate a bundle and absorb it into this store |
| `flot push [RUN] [--remote R] [--sign] [--allow-secrets]` | put a run's bundle on a git remote (default `origin`) in the ref `refs/flot/runs`, beside the runs already there; refused like `bundle` |
| `flot pull [--remote R] [--require-trusted]` | import this flow's runs from a git remote's `refs/flot/runs` that this store does not hold |
| `flot key init` / `flot key show` | create or print the host signing key |
| `flot key rotate [-o FILE]` | replace the host key, keep the old secret beside it, write a handover both keys sign |
| `flot trust add NAME KEY` / `remove` / `list` | decide which signers this host believes for this store |
| `flot trust rotate HANDOVER [--revoke]` | follow a trusted signer to the key their handover names |
| `flot mcp` | serve the same contract over MCP on stdio |

## Selecting a run

`RUN` is `latest` (the newest run executed here; an imported run is never
`latest`), `latest~N` (the run executed here N runs before it, so `latest~1` is
the previous one), a full run id, or a prefix of at least four characters of a
run id or of the root digest it ends with.

`flot run` prints one line per step. A step that executed also shows the first
and last lines of its output (the middle is marked as omitted when it exceeds
`max_inline_bytes`) and, when something was left out, the `flot log` command
that prints all of it. A reused step prints only its line; its output is still
one `flot log` away. `flot show` points each failed step at its `flot log`.

## Selecting a flow

The flow is the nearest `flot.toml` in the current directory or above it, or
exactly the file `-f` names (relative to the current directory). Paths in the
flow and the `.flot` store are relative to the flow file's directory, so one
flow always has one store, however it is named.

## Machine-readable output

`--json` gives machine-readable output for `plan`, `run`, `show`, `verify`,
`diff`, `runs`, `gc`, `bundle`, `import`, `key` and `trust`; `flot receipt`
always prints JSON. Human output shows control characters and bidirectional
overrides as visible escapes, since step output and claims can come from an
imported bundle. JSON output escapes the same characters as `\uXXXX`, which a
JSON reader decodes to the same text; `flot cat` is byte-exact.

## Exit codes

`0` passed; `1` failed, or evidence that does not authenticate (a
broken run, a tampered receipt or object, a bundle that fails its checks, a
store record that cannot be read); `2` unverified claims, a non-identical
diff, a run whose proofs differ from the revision `verify` holds it to, or a
path `flot guard` protects; `3` invalid request, including usage errors and policy refusals such as
`--require-trusted`; `4` the environment failed (IO, permissions, a store busy
with a run), so retrying can succeed.
