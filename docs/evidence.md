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

## Sharing through a git remote

`flot push` puts a run's bundle on a git remote, and `flot pull` imports the
runs a remote holds, so a CI job can publish signed runs and every clone can
fetch them without a server of its own:

```
flot push --sign            # put the latest run on origin, signed
flot pull                   # import this flow's runs from origin
flot pull --remote URL --require-trusted
```

flot has no network code: both commands run `git`, which reaches the remote
with the URL, credentials and transport it already uses (and fails rather
than prompt for a password). Runs are kept in a ref of their own,
`refs/flot/runs`, one file per run under the flow's name
(`<flow>/<run-id>.flotbundle.json`); it is never checked out and touches no
branch, index or working tree. A push builds on what the remote holds, so two
machines' runs both stay; when another push lands first, flot fetches it and
tries again. A run the remote already holds byte for byte is not pushed again.
A host with no git identity commits the ref as `flot <flot@localhost>`.

`pull` imports each run of this flow the store does not hold, exactly as
`flot import` would: authenticated in full, refused when its file name and
the run it carries disagree, and reusable only when a signer on this host's
trust list signed it. A run already here is not imported again, so a
signature added to a remote run after it was pulled is not picked up.
Refused bundles are named, and the command then exits non-zero.

## Credentials stay home

A bundle carries every byte of its run: complete output, produced files,
proof output. Before `flot bundle` or `flot push` writes or signs anything,
it looks for credential-shaped tokens in all of it, and refuses the run if
it finds one (exit 3). Only fixed shapes count: GitHub, GitLab, npm and
Slack tokens, Anthropic, OpenAI, AWS and Google keys, and PEM private keys.
Nothing is guessed from entropy, so a clean scan is not a promise that a
run holds no secret. The token is never printed, only its prefix (real
output; the token was `ghp_` and 36 characters):

```text
$ flot bundle
flot: refusing to share run 1795536384599-0beca5965629: it carries what looks like credentials, and nothing left this host:
  - step `login` receipt: GitHub token (ghp_…)
  - step `login` observation: GitHub token (ghp_…)
  - step `login` stdout: GitHub token (ghp_…)
keep them out of the step's output and run it again, or pass --allow-secrets if they are not credentials
```

A receipt is addressed by its content, so a token cannot be masked out of
the evidence; keep it out of the step's output and run the step again.
`--allow-secrets` shares the run as it is, for a token that is a fixture or
already revoked.

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
`flot sign`, `flot run --sign` and `flot bundle --sign` add an Ed25519
attestation over the run manifest, which transitively covers every receipt and
byte. The attestation is kept in the store, so `flot verify` reports it and
every later bundle of the run carries it; a CI job can sign each run as it
records it and export only the runs someone asks for. flot signs only a run
executed on this host whose evidence still authenticates: an imported run, or
one whose receipts or objects were altered, is refused. Whether to believe the
signer is the reader's decision, kept in their own trust list; even your own
signature reads as `untrusted` in your own `flot verify` until you trust your
own key:

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
  else `%APPDATA%\flot\signing.key` on Windows and `~/.config/flot/signing.key`
  elsewhere, created owner-only. On Linux and macOS flot refuses a key file
  other users can read (on Windows it relies on the profile directory's
  permissions) and replaces one only through `flot key rotate`.
- The trust list is per host as well as per store: it is sealed with this
  host's secret (see [the store](#the-flot-store)), so a `trust.json` that arrives with a copied or
  committed store is refused, not believed.

### Rotating a key

`flot key rotate` replaces the host key and writes a handover: a statement that
the new key succeeds the old one, signed by both. The old key vouches for its
successor, and the new key shows its holder took the name on, so nobody can
hand an identity to a key they do not hold. The old secret is kept owner-only
beside the new key; delete it once you no longer need it. If the handover
cannot be written, the key is not replaced.

```
host A $ flot key rotate
         rotated signing key /tmp/ka/signing.key
         new  ed25519:3857e486633665ed6e97519f141949d43677945f3f01159e795004f422f9e5a2
         old  ed25519:2d9ffad50f3fd367c7837a45405c11140c2919b41139b20870ab37f39d78a6ba
              kept as /tmp/ka/signing.key.retired-2d9ffad50f3fd367
         handover /tmp/ra/flot-handover-3857e486633665ed.json
host B $ flot import new.json         authorship: untrusted
host B $ flot trust rotate flot-handover-3857e486633665ed.json
         trusting ed25519:3857e486633665ed... as alice
         still trusting ed25519:2d9ffad50f3fd367... as alice; `flot trust remove ...` once its runs no longer matter
host B $ flot trust list
         alice            ed25519:2d9ffad50f3fd367c7837a45405c11140c2919b41139b20870ab37f39d78a6ba
         alice            ed25519:3857e486633665ed6e97519f141949d43677945f3f01159e795004f422f9e5a2
```

- A reader follows a handover only from a key they already trust; the new key
  takes the old key's name. A handover from anyone else is refused (exit 3),
  and one whose signatures do not verify is refused as altered (exit 1).
- The old key stays trusted, so runs it signed keep their authorship. For a key
  that was exposed, `flot trust rotate --revoke` stops trusting it in the same
  step.
- While a name stands for two keys, `flot trust remove NAME` asks for the key.
- A lost key cannot sign a handover. Readers then remove it and trust the new
  key by hand (`flot trust remove`, `flot trust add`), after confirming it some
  other way.

## The `.flot` store

```
.flot/
  FORMAT                     store format; another format is refused, not migrated
  .gitignore                 keeps the store out of git; share runs with bundles
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
