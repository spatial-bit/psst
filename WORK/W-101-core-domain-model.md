# W-101: Core domain model

Status: verified

## Objective

Define the I/O-free domain vocabulary, validated values, state transitions, and stable error categories needed by Slice 1. Invalid domain values must be rejected before persistence code sees them.

## Requirement mapping

- FR-002–FR-010: squad, membership, and lifecycle vocabulary.
- FR-021–FR-024: lease and availability vocabulary.
- FR-030–FR-042: direct-message, priority, acknowledgement, and idempotency rules.
- NFR-009–NFR-010: time representation boundaries and stable error codes.
- Security §8: opaque resume-token value must not implement `Display` or serialization into model-facing types; any `Debug` implementation must be unconditionally redacted.

## Dependencies

- W-000 verified.

## Allowed scope

- `crates/psst-core/**`
- Workspace dependency declarations strictly required by `psst-core`.

No filesystem, database, network, MCP, Claude, or Codex I/O. Do not design HTTP DTOs.

## Acceptance

- Typed opaque IDs exist for squad, agent, membership, instance, and message identities and cannot be accidentally interchanged.
- Validated types cover squad/member names, mission, role, message body, correlation ID, dedupe key, mode, availability, squad state, priority, and UTC timestamp/epoch conversion boundary.
- Empty or whitespace-only names, missions, and roles are rejected; message bodies enforce non-empty UTF-8 and the 64 KiB maximum.
- State-transition helpers reject archive/leave/close/acknowledge repetitions or invalid predecessors where applicable.
- Message semantic equality for a dedupe retry is explicit and includes every field whose difference must yield `idempotency_conflict`.
- Errors have stable machine codes without persistence or transport coupling.
- Secret-bearing token material is isolated from serializable/model-visible domain output.

## Tests and verification

- Table-driven boundary tests for every validated value, including Unicode and byte-length boundaries.
- Tests prove typed IDs cannot be confused through public APIs where practical; compile-fail coverage is optional.
- State-transition tests cover valid and invalid transitions.
- Idempotency equality tests change one semantic field at a time.
- Secret redaction tests prove debug formatting cannot reveal token material.
- Run `cargo test -p psst-core`, then all repository gates in `AGENTS.md`.

## Reviewer concerns

- Core must remain I/O-free and must not mirror SQLite rows as its primary abstraction.
- Byte limits must not be implemented as character counts.
- Avoid generic strings and boolean state flags where typed values or enums prevent invalid combinations.
- Do not put resume tokens in broadly serializable request/response structures.

## Verification evidence

- Independent review completed 2026-08-07; durable membership IDs replaced reusable routing names in canonical retry semantics.
- Review also closed availability construction, protocol error vocabulary, value-boundary coverage, and resume-token specification findings.
- `cargo fmt --check` passed on Windows.
- `cargo clippy --workspace --all-targets -- -D warnings` passed on Windows.
- `cargo test --workspace` passed on Windows: 17 unit tests and 0 doc-test failures.
