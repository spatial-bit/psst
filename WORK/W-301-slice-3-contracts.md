# W-301: Slice 3 CLI, profile, and MCP contracts

Status: verified

## Objective

Freeze the Slice 3 interface and authority contracts before implementing credential persistence, CLI behavior, or MCP transport.

## Requirement mapping

- FR-001–FR-010, FR-020–FR-024, FR-030–FR-043 as exposed through CLI/MCP.
- FR-070–FR-072, PRD §§8, 12–15, and Slice 3 gate in §20.
- ADR 0004.

## Dependencies

- Slice 2 verified and merged to `main` at `d8d6d88`.

## Allowed scope

- ADR and checked-in CLI help/JSON/MCP schema fixtures.
- Shared non-secret application DTOs and error taxonomy required to express the contract.
- Dependency/MSRV spike for a pinned crates.io MCP SDK.

Do not persist credentials, start heartbeat tasks, implement command behavior, call a relay from MCP, or add harness activation.

## Acceptance

- The `psst` command tree, stable JSON success/error envelopes, stdout/stderr rules, and exit classes are explicit.
- All nine MCP tools have closed input/output schemas, stable safe errors, annotations, and no secret-shaped fields.
- Configuration precedence, platform path roles, profile identity, ownership lock, and credential authority are explicit.
- One MCP process owns one startup-selected profile; cooperative mode and internal client metadata are adapter-controlled.
- Participant content has a structured untrusted-data representation that cannot be escaped by adversarial message text.
- The chosen MCP SDK/version builds at Rust 1.89 on Windows, Linux, and macOS, or a reviewed minimal protocol implementation is selected instead.
- Slice 4 exclusions are mechanically searchable: no client launch, Channels, App Server, keystroke injection, wake loop, or `claude -p` path.

## Verification evidence

- Windows implementation gate on Rust 1.89.0: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets --locked -- -D warnings`, and
  `cargo test --workspace --locked` passed on 2026-08-08 using an isolated target directory because
  a concurrent reviewer owned the workspace target lock.
- The workspace suite includes 174 tests: seven application contract/golden/security tests, two MCP
  metadata/framing tests, and three child-process stdio tests covering handshake, protocol failure, exact
  1 MiB framing failure, fixed diagnostics, hostile-content non-reflection, and bounded cleanup.
- Exact `rmcp = 3.1.2` builds on Rust 1.89. Its direct feature surface remains
  `server`, `transport-io`, and `macros`; macros are retained for the immediately following W-306
  checked-in tool-server implementation and introduce no activation behavior.
- Two independent adversarial reviews approved the final contract and bounded-transport design.
- Revision `83fa79a` passed GitHub Actions on Windows, Linux, and macOS, including the native
  Windows x86-64, Linux x86-64, and macOS ARM64 development-artifact jobs:
  <https://github.com/spatial-bit/psst/actions/runs/31246729089> and
  <https://github.com/spatial-bit/psst/actions/runs/31246729075>.
