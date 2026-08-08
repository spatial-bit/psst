# W-301: Slice 3 CLI, profile, and MCP contracts

Status: active

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

Pending.
