# Progress

Current slice: 2 — Relay and typed client
Current gate: Slice 1 satisfied on merged `main`; Slice 2 decomposed for execution
Last reconciled: 2026-08-07

## Verified

- Product and engineering PRD drafted.
- Autonomous build-loop controls drafted.
- W-000 repository foundation published at `spatial-bit/psst`.
- Cross-platform formatting, linting, and tests passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31209799830>.
- W-101 core domain model independently reviewed and verified: typed identities and values, state rules, durable idempotency semantics, stable errors, secret redaction, and 17 passing tests.
- W-101 cross-platform PR evidence passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31214329560>.
- W-102 SQLite foundation independently reviewed and locally verified: bundled SQLite, fail-closed embedded migrations, exact schema constraints, rollback/restart durability, and concurrent startup.
- W-102 cross-platform PR evidence passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31215449960>.
- W-103 squad and membership transactions independently reviewed and locally verified: durable lifecycle, immutable leave authority, roster projection, concurrency, rollback, restart, and stable errors.
- W-103 cross-platform PR evidence passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31216646126>.
- W-104 instance leases and resume independently reviewed and locally verified: exclusive ownership, deterministic heartbeat, 256-bit OS-generated credentials, atomic resume, restart, rollback, and non-disclosure.
- W-104 cross-platform PR evidence passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31217748580>.
- W-105 durable message submission independently reviewed and locally verified: persist-before-success, scoped idempotency, ambiguous-commit recovery, authorization, integrity metadata, concurrency, restart, and stable errors.
- W-105 cross-platform PR evidence passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31218581813>.
- W-106 inbox and acknowledgement independently reviewed and locally verified: cursorless replay, sequence order, bounded indexed reads, recipient-only atomic acknowledgement, restart, rollback, and corruption handling.
- W-106 cross-platform PR evidence passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31219378486>.
- W-107 Slice 1 reliability gate independently approved: 80 tests, repeated stress, real held-lock contention, mid-ack rollback, and abrupt-process commit-boundary evidence.
- W-107 revision `4c8e094` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31220050245>.
- Slice 1 merged to `main` at `fe4f0bf`; the merged revision passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31220421510>.
- W-201 protocol/OpenAPI contract independently reviewed and verified; GitHub Actions passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31223796106>.
- W-202 authenticated store boundary and relay runtime independently approved with 126 passing tests; revision `b99f9c4` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31227930335>.
- W-203 lifecycle HTTP API independently approved with 130 passing tests; revision `1d2a2ca` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31229928971>.
- W-204 message, inbox, acknowledgement, and transcript HTTP API independently approved with 137 passing tests; revision `7591018` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31231619550>.
- W-205 bounded long polling independently approved with 146 passing tests; revision `2273849` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31233081040>.
- W-206 typed Rust client independently approved with 158 passing tests: complete Slice 2 API coverage, memory-only redacted credentials, bounded transport, total operation deadlines, conservative retries, exact prepared-send idempotency, real ambiguous-commit recovery, and restart/offline/resume E2E evidence.
- W-206 revision `f1db97c` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31235067734>.
- W-207 Slice 2 reliability gate independently approved with 162 passing tests, 100 production TCP watchers, exact restart reconciliation, bounded disconnect/shutdown/lock recovery, and repeated release performance above 100 messages/second with p95 below 100 ms.
- W-207 revision `9bded29` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31237113554>.

## Active

- W-208 cross-platform development artifacts.

## Ready

- Slice 3 CLI and cooperative MCP work is ready after W-208.

## Gate evidence

- Windows local formatting: passed
- Windows local lint: passed
- Windows local tests: passed
- GitHub Actions matrix: passed on Windows, Linux, and macOS

## Next coordinator action

Produce W-208 cross-platform development artifacts and verify clean local relay startup/installability before closing Slice 2.
