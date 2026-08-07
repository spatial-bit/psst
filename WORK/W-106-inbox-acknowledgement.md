# W-106: Inbox retrieval, replay, and acknowledgement

Status: verified

## Objective

Implement bounded inbox reads and atomic per-message acknowledgement while preserving the distinction between retrieval and processing.

## Requirement mapping

- FR-036–FR-039, FR-041–FR-043.
- NFR-002–NFR-006, NFR-009–NFR-010.
- PRD §9 message lifecycle and §10 atomic acknowledgement batch.

## Dependencies

- W-105.

## Allowed scope

- `crates/psst-core/**` inbox query/result validation only if required.
- `crates/psst-store/**` inbox and acknowledgement operations/tests.

No HTTP long polling or in-memory wake mechanism; this unit implements authoritative store reads only.

## Acceptance

- Pending inbox reads are acknowledgement-driven, accept no sequence cursor, and enforce limits of 1–100 messages and at most 1 MiB serialized-equivalent content.
- Pending messages sort in stable ascending sequence order. Priority is retained in envelopes for wake metadata but does not change inbox pagination order.
- Reading never changes `acknowledged_at`, even across repeated reads and restart.
- Acknowledgement accepts an individually enumerated batch, is atomic, and affects only messages addressed to that membership.
- Acknowledgement is idempotent; unknown or foreign message IDs fail according to an explicit stable policy without partial updates.
- Unacknowledged messages replay after process/store restart; acknowledged messages do not reappear in the pending inbox.
- Transcript/history reads needed solely to verify preservation may be internal test helpers, not premature public protocol APIs.

## Tests and verification

- Retrieve twice without acknowledgement and compare stable IDs and sequences.
- Crash/reopen before acknowledgement and verify replay.
- Acknowledge, reopen, and verify pending exclusion plus durable history.
- Mixed valid/foreign/unknown acknowledgement batch proves atomic rollback.
- Ascending ordering, acknowledgement-driven batching, message-count bound, and aggregate-size bound tests.
- Run focused store/core tests, then all repository gates.

## Reviewer concerns

- Pending inbox has no cursor; sequence cursors are reserved for immutable transcript/history APIs.
- Aggregate output bounding must account for envelope overhead conservatively.
- Priority must not alter pending inbox order or create skip conditions.
- No global acknowledgement or sender-authorized acknowledgement path.

## Verification evidence

- Independent review completed 2026-08-07; approval followed bounding acknowledgement batches at 100 before transaction creation and adding a sequence-oriented pending-inbox index.
- The production pending query has an `EXPLAIN QUERY PLAN` assertion proving use of `messages_inbox_order` without a temporary sort.
- `cargo fmt --check` passed on Windows.
- `cargo clippy --workspace --all-targets -- -D warnings` passed on Windows.
- `cargo test --workspace` passed on Windows: 17 core tests, 59 store tests, and all doc tests.
- Evidence includes repeated read, crash/reopen replay, durable acknowledgement, strict ascending order regardless priority, count/byte bounds, oversized-batch no-mutation, mixed invalid/foreign atomic rollback, idempotent timestamps, corruption detection, and migration upgrade.
