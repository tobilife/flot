# Changelog

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
