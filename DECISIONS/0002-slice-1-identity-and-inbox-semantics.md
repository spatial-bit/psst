# ADR 0002: Identity continuity and acknowledgement-driven inboxes

Status: accepted

## Context

Independent domain and SQLite reviews found two unsafe ambiguities in the initial PRD.

First, automatically releasing an agent name when a 30-second presence lease expires would permit identity and mailbox takeover after ordinary sleep or network interruption. Second, filtering a pending inbox with `after=sequence` can permanently hide a retrieved but unacknowledged message after a crash. Priority-first ordering combined with a scalar sequence cursor creates another skip condition.

## Decision

- Lease expiry makes an instance offline but does not release its durable membership name.
- A membership is resumed only by presenting its opaque resume token. Explicit leave releases the name. Administrative takeover is deferred.
- Squad names are lowercase ASCII slugs. Membership routing names are lowercase ASCII identifiers. Rich display labels may be added later but are never routing authority.
- Initial joins create new agent identities. Cross-squad durable agent identity reuse is deferred.
- Message bodies are non-empty UTF-8 and limited by bytes.
- Adapters generate a dedupe key for every logical send. Idempotency is guaranteed only when a dedupe key is present.
- The canonical idempotency comparison includes squad, sender, recipient, exact body bytes, priority, reply target, and correlation ID.
- Pending inbox reads are acknowledgement-driven. They return unacknowledged messages in ascending sequence order and do not filter by a retrieval cursor.
- Priority affects wake metadata only in version one. Agents may choose processing order after retrieval.
- `after=sequence` remains valid for immutable transcript/history pagination, not pending inbox retrieval.
- Only a message's recipient membership may acknowledge it. Batch acknowledgement is atomic and idempotent.
- Sender, recipient, and reply squad consistency is enforced with composite database constraints in addition to application validation.

## Consequences

The relay may replay messages indefinitely until acknowledgement, which is the intended at-least-once contract. Losing a resume token requires choosing a different name or an explicit future administrative recovery feature; this is preferable to silent identity transfer. The v1 inbox protocol remains small and cannot skip pending mail through cursor advancement.
