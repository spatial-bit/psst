# W-106: Inbox retrieval, replay, and acknowledgement

Status: pending

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

- Inbox reads accept an exclusive `after` sequence and enforce limits of 1–100 messages and at most 1 MiB serialized-equivalent content.
- Eligible high-priority messages sort before normal messages, with stable sequence order within a priority.
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
- Ordering, pagination, message-count bound, and aggregate-size bound tests.
- Run focused store/core tests, then all repository gates.

## Reviewer concerns

- Cursor is pagination only and must never act as acknowledgement.
- Aggregate output bounding must account for envelope overhead conservatively.
- Priority ordering must not make `after` semantics skip lower-priority messages; document/query-test the chosen pagination contract.
- No global acknowledgement or sender-authorized acknowledgement path.

