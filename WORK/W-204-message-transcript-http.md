# W-204: Durable messaging, acknowledgement, and transcript HTTP API

Status: active

## Objective

Expose durable send, immediate pending-inbox retrieval, atomic acknowledgement, and immutable transcript history through authenticated versioned HTTP endpoints.

## Requirement mapping

- FR-030–FR-043.
- NFR-001–NFR-006, NFR-009, NFR-010.
- PRD §11 message, inbox, acknowledgement, and transcript endpoints.

## Dependencies

- W-203.

## Allowed scope

- Message/inbox/ack/transcript handlers and protocol DTO refinements.
- A bounded transcript store query and index only if absent from Slice 1.
- HTTP contract and real-relay integration tests.

Do not implement waiting inbox behavior, notification registries, client retries, CLI, MCP, or wake activation.

## Acceptance

- Send validates authenticated sender authority, persists before success, and preserves exact idempotent retry/conflict semantics.
- Immediate `wait=0` inbox reads are acknowledgement-driven, ascending sequence, and bounded by message count and serialized response size.
- Retrieval never acknowledges; acknowledgement is recipient-only, atomic, bounded, and idempotent.
- Transcript is immutable history ordered by sequence and uses an exclusive `after` cursor without changing pending-inbox semantics.
- Offline recipients and relay restart preserve mail and replay behavior.
- Oversized requests/responses and invalid cross-squad references return stable non-sensitive errors.

## Tests and verification

- HTTP offline-delivery/restart/replay/acknowledgement scenario using real files.
- Timeout-after-commit simulation followed by HTTP retry with one durable row.
- Cursorless inbox versus cursor-based transcript contract tests.
- Count, byte, body, batch, and malformed-request boundary tests.
- Concurrent duplicate send and acknowledgement atomicity tests.
- Run all repository gates.

## Reviewer concerns

- The handler must not notify any waiter before commit; notification belongs to W-205.
- Response-size accounting must remain conservative after JSON envelope overhead.
- Transcript authorization must be squad-scoped.
- Do not accidentally turn a read timeout/cancellation into acknowledgement.

## Verification evidence

Pending.
