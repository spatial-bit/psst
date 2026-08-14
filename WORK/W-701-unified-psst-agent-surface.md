# W-701 — Unified psst process and agent-driver boundary

Status: local implementation candidate; native CI and packaged rehearsal pending

## Objective

Make `psst` the single normal executable for operator, relay, Claude, Codex, and internal MCP
process roles while preserving the already-approved lifecycle and protocol boundaries.

## Acceptance

- `psst agent claude [--continue] [--dangerously-skip-permissions]`, `psst agent codex
  [--continue]`, and `psst agent status` have closed grammar and stable help.
- Generated Claude configuration invokes the absolute current `psst internal mcp` command, stays
  outside the user's project, contains no credential, and launches only interactive Claude Code.
- Codex creates one durable task record when absent, resumes it thereafter, and uses the current
  `psst internal mcp` command for each turn. `--continue` fails closed if no record exists.
- The hidden MCP mode preserves protocol-only stdout, bounded framing, clean shutdown, and all nine
  frozen tools. Compatibility binaries use the same implementation.
- Local command discovery is bounded, supports native executables and Windows command shims, and
  accepts an explicit non-secret path override.
- Tests cover grammar, hidden-mode purity, durable identity, generated arguments/configuration,
  compatibility behavior, and supported-platform process lifecycle.
- Full format, strict workspace Clippy, and workspace tests pass before handoff. Native claims await
  CI and packaged rehearsal.

## Evidence

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed.
- `cargo test --workspace`: passed on Windows, including 21 CLI unit tests, the real
  `psst internal mcp` process handshake, 13 Codex unit tests with one explicit installed-client
  fixture ignored, 13 MCP unit tests, and the complete existing workspace suites.
- The generated Claude configuration, launcher lock, Windows command-shim forwarding, bounded Codex
  task record, App Server process configuration, and hidden-mode process purity have focused
  regression coverage.
- Cross-platform native CI and clean-downloaded packaged launch evidence remain required before this
  work unit can be called verified.
