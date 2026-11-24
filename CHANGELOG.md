# Changelog

## 0.2.1

- `flot key rotate` replaces the signing key and writes a handover that the
  old and the new key both sign; the old secret is kept owner-only beside the
  new key. If the handover cannot be written, the key is not replaced.
- `flot trust rotate HANDOVER [--revoke]` follows a trusted signer to their
  new key under the same name. A handover from a key this store does not trust
  is refused, and one whose signatures do not verify is refused as altered.
  The old key stays trusted unless `--revoke`.
- `flot trust remove NAME` asks for the key when the name stands for two.
- The README is also available in Chinese; every release updates the
  English, Korean and Chinese READMEs together, and a test checks that each
  names the release.

## 0.2.0

- `flot sign [RUN]` and `flot run --sign` sign a run executed here without
  exporting it; the attestation is kept in the store, so `flot verify` reports
  it and every later bundle carries it. A run that was imported, or whose
  evidence no longer authenticates, is never signed; `flot bundle --sign` now
  refuses the latter too. `run --sign` checks for the key before anything runs.
- Releases ship prebuilt binaries for Linux (x86_64, aarch64; static) and macOS
  (x86_64, arm64) with checksums, built and published by CI.

## 0.1.1

- MCP tools carry annotations: every tool but `flot_run` is read-only, so hosts
  such as Codex let them through without asking. `flot_run` is marked
  destructive and open-world, since it runs the flow's unsandboxed commands.
- The cost-ceiling limitation names the steps that reported no metrics.
- Examples run each agent step apart from the user's own Claude Code setup
  (`--setting-sources project`, `--strict-mcp-config`, only the tools the step
  needs), which cut a three-reviewer run from $3.25 to $0.42.
- `examples/parallel-review.toml` reviews staged and unstaged changes
  (`git diff HEAD`).
- Documentation: budget defaults, MCP approvals, and runs tested with Claude
  Code and Codex.

## 0.1.0

First release.
