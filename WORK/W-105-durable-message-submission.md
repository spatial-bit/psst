# W-105: Durable message submission and idempotency

Status: pending

## Objective

Implement atomic durable direct-message submission with recipient authorization, stable sequence assignment, priority, replies, correlation, and conflict-safe retry deduplication.

## Requirement mapping

- FR-030–FR-035, FR-040–FR-042.
- NFR-001–NFR-006, NFR-009–NFR-010.
- PRD §9 message lifecycle and §10 atomic idempotency transaction.

## Dependencies

- W-104.

## Allowed scope

- `crates/psst-core/**` message semantic helpers.
- `crates/psst-store/**` message submission operations/tests.

Do not implement inbox reads, acknowledgement, long polling, HTTP, or wake notification.

## Acceptance

- Only a non-left sender in an active squad can send to a non-left recipient in the same squad.
- Offline recipients remain valid; unknown, left, cross-squad, and archived-squad cases return stable errors.
- Success is returned only after the insert transaction commits.
- IDs are stable and SQLite assigns a monotonic sequence.
- A repeated sender/squad dedupe key with identical semantics returns the original message result and creates no row.
- Reuse with any changed semantic field returns `idempotency_conflict`.
- Reply targets must exist and belong to the same squad; message body and size limits are enforced before write.
- Message rows are immutable through the public store API.

## Tests and verification

- Timeout-after-commit simulation followed by retry yields one durable row and the original ID/sequence.
- Change each semantic input under the same dedupe key and verify conflict.
- Concurrent duplicate-send test through independent connections yields one row.
- Offline, left, unknown, cross-squad, archived, invalid reply, high-priority, Unicode, and size-boundary tests.
- Restart persistence and sequence monotonicity tests.
- Run focused store/core tests, then all repository gates.

## Reviewer concerns

- Idempotency check and insert must be one transaction and backed by a unique partial index.
- Body hash is integrity/semantic metadata, not authentication; canonical comparison must not omit fields.
- Persist-before-wake must be structurally possible: no callback or notification before commit.
- Avoid leaking SQL constraint names through public errors.

