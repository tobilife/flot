# Contributing to flot

Bug reports, feature requests and pull requests are welcome. For anything
larger than a fix, open an issue first so the design can be agreed before the
code is written.

## Development setup

You need Rust 1.88 or later and a POSIX shell. Linux and macOS are supported.

```bash
git clone https://github.com/tobilife/flot
cd flot
cargo build
```

## Checks

A pull request must pass the same checks as CI:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

flot also gates itself: `cargo run -- run` runs [`flot.toml`](flot.toml), which
proves each of those checks and records receipts in `.flot/`.

## Rules the code keeps

- **Layers point down.** `core` → `store` → `engine` → `service` → `adapters`.
  `core` performs no IO, adapters never import each other, and both adapters
  call the one `Workspace` contract. `tests/architecture.rs` enforces this.
- **Every fix comes with a test that fails without it.** Reproduce the problem
  first, then check that removing the guard makes the new test fail.
- **Never weaken the claim gate.** A change must not let a claim become
  `verified`, or a run `passed`, without a proof that ran and exited zero.
- **Evidence formats are versioned.** A change to what a receipt, run or bundle
  records bumps its schema or the store format; flot refuses old formats rather
  than migrating them.
- **Documentation tracks behavior.** Update [README.md](README.md) and
  [docs/](docs) in the same change, and only show output that a real run
  printed.

## Releasing

1. Rename `## Unreleased` in [CHANGELOG.md](CHANGELOG.md) to the new version,
   and set the same `version` in `Cargo.toml`.
2. Commit, then push a tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.

The [release workflow](.github/workflows/release.yml) checks that the tag,
`Cargo.toml` and the changelog agree, runs the tests, builds binaries for
Linux (x86_64, aarch64; static musl) and macOS (x86_64, aarch64), publishes
them with checksums on a GitHub release whose notes are the changelog section,
and publishes the crate through crates.io trusted publishing.

## Reporting security issues

Do not open a public issue for a vulnerability; follow [SECURITY.md](SECURITY.md).
