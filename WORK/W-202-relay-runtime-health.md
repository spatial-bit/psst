# W-202: Authenticated store boundary, relay runtime, and health

Status: verified

## Objective

Complete the atomic credential-authorized store boundary, then create the bounded HTTP relay runtime with safe configuration, health/readiness, diagnostics, and a dedicated SQLite execution boundary.

## Requirement mapping

- NFR-004–NFR-007, NFR-009, NFR-010.
- Security §8 default loopback and trusted-LAN warning.
- PRD §15 configuration and §16 diagnostics/operations.

## Dependencies

- W-201.

## Allowed scope

- `crates/psst-store/**` atomic join-and-claim, authenticated protected commands, readiness, transcript foundation, and checkpoint operations/tests.
- `crates/psst-relay/**`, workspace manifests, focused integration-test support, and configuration types outside core.

Do not expose product mutation endpoints beyond test-only wiring, implement long polling, build the typed client, add LAN discovery, or add CLI/MCP behavior.

## Acceptance

- Relay defaults to loopback and requires an explicit option for LAN binding; LAN startup emits the trusted-network warning.
- Configuration has documented deterministic defaults and enforces request-body, connection, timeout, and concurrency bounds.
- SQLite operations run behind a bounded execution mechanism; queue saturation returns a stable retryable error rather than growing memory without bound.
- Join and initial claim commit atomically. Credential verification, lease/ownership validation, and each protected operation execute in one store transaction or authoritative query boundary; handlers cannot supply an acting membership independently.
- Authenticated store commands cover heartbeat, send, pending inbox, acknowledgement, transcript, leave, archive, and resume without revealing whether an instance ID or token was incorrect.
- `/healthz` is mutation-free process liveness; `/readyz` verifies store access and migration compatibility.
- Logs can be text or JSON, carry request/correlation fields, omit bodies at info level, and redact all credentials.
- Shutdown stops accepting requests, cancels/drains bounded work, checkpoints SQLite, and exits within a documented testable bound.

## Tests and verification

- Real-file readiness tests for healthy, busy, incompatible, and unavailable stores.
- Atomic join/claim failure and credential-authorized operation tests, including expiry, ownership replacement, wrong-token concealment, and concurrent leave/resume/send races.
- Bound-address and trusted-LAN-warning tests.
- Queue saturation, request timeout, and cancellation tests with no leaked tasks.
- Secret/body log-capture tests.
- Graceful shutdown test with in-flight non-waiting work.
- Run all repository gates.

## Reviewer concerns

- `rusqlite::Connection` thread-affinity and serialized writer semantics must be explicit.
- Do not split authentication and protected work across store commands.
- Avoid one unbounded `spawn_blocking` call per request.
- Health must not imply database readiness.
- Shutdown ownership should be designed before long polls are added.

## Verification evidence

- Independently reviewed and approved locally on 2026-08-07 after credential validation, authoritative time, exact-retry lifecycle, checkpoint, shutdown, connection-bound, logging, and readiness findings were resolved.
- `cargo fmt --check` passed on Windows.
- `cargo clippy --workspace --all-targets -- -D warnings` passed on Windows.
- `cargo test --workspace` passed on Windows: 17 core, 18 protocol, 18 relay, and 73 store tests; all doc tests passed.
- Store evidence covers atomic bootstrap rollback at every write boundary, every protected command's credential/expiry behavior, resume ownership, exact retries after expiry/leave/archive, and repeated independent-connection lifecycle races.
- Runtime evidence covers queue saturation/recovery/cancellation, actual TCP and request admission bounds, body/deadline enforcement, healthy/unavailable/incompatible readiness, WAL writer behavior, checkpoint failure propagation, real HTTP drain/refusal, process-hard deadline exit, and credential/body-safe text and JSON logs.
- Revision `b99f9c4` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31227930335>.
