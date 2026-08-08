# W-206: Typed Rust client and retry boundaries

Status: verified

## Objective

Implement a small typed Rust client for the complete Slice 2 API with explicit timeouts, cancellation, credential isolation, and safe idempotent retry behavior.

## Requirement mapping

- FR-001–FR-010, FR-020–FR-024, FR-030–FR-043 as client operations.
- NFR-003–NFR-005, NFR-009, NFR-010.
- Security §8 token transport/non-disclosure and PRD §14 `psst-client` boundary.

## Dependencies

- W-205.

## Allowed scope

- `crates/psst-client/**`, shared protocol types, fake-server tests, and real-relay integration tests.
- An in-memory secret credential holder and injectable HTTP transport/time source for deterministic tests.

Do not implement durable token files (Slice 3), heartbeat background loops, CLI, MCP, LAN discovery, or activation.

## Acceptance

- Every Slice 2 endpoint has a typed operation with stable error decoding and explicit request/connect/long-poll timeouts.
- Credentials are applied only in the agreed authorization header and are redacted from `Debug`, display, diagnostics, and errors.
- Client-generated dedupe keys are present by default for sends; retry after ambiguous transport failure preserves the same key.
- Automatic retries are limited to safe reads and idempotent operations, use bounded attempts/backoff, honor cancellation, and never retry semantic non-retryable errors.
- Long-poll timeout budgets account for server wait plus bounded transport margin.
- No unbounded response buffering, connection pool, queue, or retry loop is introduced.

## Tests and verification

- Fake-server request-shape, header, timeout, malformed-response, and stable-error tests.
- Ambiguous send retry proves the same dedupe key and one committed message against a real relay.
- Cancellation and bounded retry/backoff tests without wall-clock sleeps.
- Credential leakage scan across debug/error/log captures.
- Full typed-client lifecycle and offline-mail scenario against a restarted relay.
- Run all repository gates.

## Reviewer concerns

- Separate retry policy from endpoint methods and classify failures conservatively.
- Never clone or serialize credentials unnecessarily.
- Do not claim token-at-rest protection before Slice 3.
- Client DTOs should come from the shared contract rather than parallel hand-written shapes.

## Verification evidence

- Revision `f1db97c` independently approved after typed-client contract and implementation review.
- All 158 workspace tests passed locally on Windows; formatting, strict Clippy, documentation, and diff checks passed.
- Real relay/SQLite tests cover the full lifecycle, restart/resume, offline mail, bounded responses, cancellation, and exact-key retry after an ambiguous committed send.
- The Windows-only CI timing failure was traced to test scheduling headroom, corrected without changing production behavior, and independently approved after ten consecutive focused Windows passes.
- GitHub Actions passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31235067734>.
