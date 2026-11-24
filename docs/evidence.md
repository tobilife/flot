# Evidence, bundles and trust

## Bundles

`flot bundle` writes a run, its receipts and every object they reference into
one JSON file. `flot import` re-checks all of it before the first write:
objects must hash to their digests, receipts to their ids, every referenced
object must be present and no unreferenced one may come along, and the run
root must fold back from its steps. A bundle that fails any check is refused
whole (exit `1`) and leaves the store untouched. Bundles over 1 GiB are refused
before they are read.

Everything a manifest says that follows from its receipts is recomputed, not
read: its run id, each step's status and claim counts, the totals, the verdict
and the root. A bundle whose manifest says `passed` over a refuted claim is
refused, and the same check makes `flot verify` report a run edited in place as
broken. Run ids name files, so one that is not exactly what flot derives is
refused before it can reach a path.

A reviewer can audit a claimed result from the bundle alone, without trusting
the agent or the host that produced it.

## Evidence is not cache

 Anyone can seal an authentic receipt claiming any
outcome: integrity proves a receipt was not altered, not that its work
happened. Reusing a receipt skips the work and writes the receipt's output
files into the workspace, where the next step may execute them. So an import
only seeds the cache when a signer on the local trust list signed the bundle,
only with receipts local work would reuse (settled successes), and never in
place of a cache entry the store already relies on. An unsigned or untrusted
bundle is evidence to inspect and verify, and local runs execute as if it were
not there. A cache entry seeded by an import remembers who vouched for it and
is honored only while one of them is still trusted: removing a signer also
stops reuse of what they signed.

An imported run is marked `(imported)` in `flot runs` and `flot show`, is never
`latest`, and is never re-signed by this host: `flot bundle --sign` on it is
refused, while `flot bundle` carries its original signatures forward. An import
never replaces a run or a receipt the store already holds. With a trusted
signer:

```
host A $ flot run --jobs 3            root d7b7703db719  1078ms
host A $ flot key init
host A $ flot bundle --sign -o demo.json
         wrote demo.json (5 receipts, 11 objects, 14345 bytes)  root d7b7703db719
host B $ flot import demo.json        authorship: untrusted
         not reusable: only a trusted signer's receipts can stand in for local work
host B $ flot trust add alice ed25519:db30b8492e7f...
host B $ flot import demo.json        authorship: trusted
         reusable: local runs may reuse these receipts and their outputs
host B $ flot run                     5 cached  root d7b7703db719  21ms
host B $ flot runs
         1795501571270-d7b7703db719  passed     5 steps  claims 1v/0u/0r
         1795501569894-d7b7703db719  passed     5 steps  claims 1v/0u/0r  (imported)
host C $ ... same import as host B, then:
host C $ flot trust remove alice
host C $ flot run                     5 ran
         - a: cached receipt came from a signer this store no longer trusts; not reused
```

## Who produced it

Integrity says the evidence was not altered; it does not say who produced it.
`flot bundle --sign` adds an Ed25519 attestation over the run manifest, which
transitively covers every receipt and byte. Whether to believe the signer is
the importer's decision, kept in their own trust list:

```
host A $ flot key init
         ed25519:db30b8492e7f137d34c9119e15a91aaaf9e871f9adaf7ce13ea0a70df8c02b49
host A $ flot bundle --sign -o demo.json
host B $ flot import --require-trusted demo.json
         flot: bundle is untrusted and a trusted signer is required; ...   (exit 3)
host B $ flot trust add alice ed25519:db30b8492e7f...
host B $ flot import --require-trusted demo.json
         authorship: trusted
           + ed25519:db30b8492e7f137d  trusted as alice
```

- A signature that does not verify refuses the bundle whole: something changed
  after signing, even if every receipt and digest still checks out.
- A valid signature by a key you have not trusted is reported as `untrusted`,
  never upgraded. Without `--require-trusted` it still imports, labelled.
- `flot verify` and MCP `flot_verify` re-judge stored signatures against the
  current trust list, so revoking a signer changes the authorship they report
  for runs already imported (the exit code still reflects integrity), and stops
  reuse of their receipts.
- Weak (small-order) keys are refused; verification uses `verify_strict`.
- The key lives at `$FLOT_SIGNING_KEY`, else `$XDG_CONFIG_HOME/flot/signing.key`,
  else `~/.config/flot/signing.key`, created owner-only. flot refuses a key file
  other users can read and never replaces an existing one.
- The trust list is per host as well as per store: it is sealed with this
  host's secret (see [the store](#the-flot-store)), so a `trust.json` that arrives with a copied or
  committed store is refused, not believed.

## The `.flot` store

```
.flot/
  FORMAT                     store format; another format is refused, not migrated
  objects/<ab>/<digest>      complete stdout/stderr, produced files, proof output,
                             per-run observations
  receipts/<digest>.json     one receipt per distinct piece of work
  runs/<run-id>.json         run manifest: ordered receipts, verdict, root digest
  attestations/<run-id>.json signatures over a run manifest
  imported/<run-id>.json     marks a run that arrived in a bundle
  cache/<step-key>.json      step key -> receipt, sealed by this host
  trust.json                 signers this host believes, sealed by this host
  latest                     id of the newest run executed here
  lock, tmp/                 run/gc exclusion and atomic writes
```

Two kinds of record live here. **Evidence** authenticates itself wherever it
goes: objects and receipts are named by digests of their content, a file that
holds anything but the record its name says is refused, and a manifest is
re-derived from its receipts. **Decisions** do not travel: the cache (which
receipt may stand in for running a step) and the trust list (whose signatures
are believed) are sealed with a secret that never leaves this host
(`host.secret`, kept beside the signing key and created owner-only on first
use). A store that is copied, committed or downloaded brings its evidence,
which still audits, and none of its decisions: its cache entries are ignored
and its trust list is refused until you remove it and trust signers yourself.

A cache hit is only honored when the entry carries this host's seal, the
receipt parses, hashes to its own id, carries the requested step key, records
a settled success, makes the claims the step makes today, and still has every
object it references intact — a cache that can lie is worse than no cache. Only
settled success is reusable: failed, timed-out and cancelled steps, and steps
whose proof was refuted or did not finish, are always retried, so a transient
failure never becomes a permanent one. A claim with no proof command does not
block reuse, because re-running cannot change it.

Nothing is created on disk until something is recorded, so reading a project
leaves no store behind. Runs and imports hold the store lock shared; `flot gc`
takes it exclusively and refuses while a run is in progress, and stops without
collecting anything if a run or receipt cannot be read, since it cannot know
what that record references.
