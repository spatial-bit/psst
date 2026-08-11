# ADR 0003: Slice 2 wire, authentication, and runtime boundaries

Status: accepted

## Context

Slice 1 exposes durable operations in terms of membership and instance IDs. An HTTP relay cannot safely authenticate a credential in one store operation and mutate in another: lease expiry, leave, or ownership replacement between those calls would create a time-of-check/time-of-use authorization defect. The PRD also requires resume but its initial endpoint inventory omitted a resume route, and join must provision a secret without making that secret part of later model-facing data.

## Decision

- The public API is rooted at `/v1`. Request and response DTOs are distinct from core and store records.
- `POST /v1/squads/{squad}/resume` is the explicit resume route.
- Session credentials use `Authorization: Bearer <instance-id>.<resume-token>`. Credentials are forbidden in URLs, logs, errors, tracing fields, and ordinary JSON bodies.
- Join is the unauthenticated bootstrap operation. Join and initial instance claim are one atomic store operation. The one-time credential is returned in the `Psst-Session-Credential` response header; the JSON body contains only non-secret membership, instance, and lease metadata.
- Resume authenticates the prior credential, atomically closes the predecessor and creates the new instance, and returns the replacement credential in the same response header. Resume tokens remain stable continuity secrets while instance IDs rotate.
- Heartbeat, send, pending inbox, acknowledgement, transcript, leave, and archive derive acting identity from the authenticated session. Request bodies cannot select another sender or mailbox owner.
- Roster reads require historical authentication and must match the credential's squad. This keeps archived and left-member history readable without permitting a profile bound to one squad to select another squad's roster.
- Credential verification, active-membership validation, lease validation, and each protected read or mutation occur within one authoritative store command/transaction boundary. Invalid instance IDs and wrong tokens are concealed as `not_found`; a correctly authenticated expired lease returns `lease_expired`.
- List and describe squad are unauthenticated trusted-LAN reads. Squad creation is unauthenticated to preserve zero-friction bootstrap. Archive requires an active authenticated membership in that squad. There is no administrator role in version one.
- Transcript access requires an active authenticated membership in the squad and uses an exclusive sequence cursor. Pending inbox remains cursorless and acknowledgement-driven.
- API timestamps are UTC RFC 3339 with exactly millisecond precision. Integer message sequences remain JSON integers.
- Mutation requests reject unknown JSON fields. Responses may evolve additively; clients ignore unknown response fields. Unknown enum values fail predictably.
- Exact message retries keep the same scoped dedupe behavior defined by ADR 0002. Acknowledgement retries are safe and idempotent when the same authenticated recipient and ID batch are used.
- Stable HTTP mapping is: `invalid_request` 400, concealed `not_found` and `recipient_not_found` 404, `not_member` 403, `payload_too_large` 413, lifecycle/name/idempotency conflicts 409, `rate_limited` 429, `database_busy` 503, and `internal_error` 500.
- The relay owns one dedicated SQLite worker thread behind a bounded command channel. Queue saturation maps to retryable `rate_limited`; arbitrary closures and one blocking task per request are not permitted.
- Long-poll notifications are post-commit hints only. A waiter subscribes before querying, queries SQLite before sleeping and after every signal, and also exits on timeout, disconnect, or shutdown.
- Default binding is loopback. LAN binding is explicit and emits the trusted-LAN/no-TLS warning. Discovery and TLS are outside Slice 2.

## Consequences

The store gains atomic join-and-claim plus authenticated variants of protected operations before handlers are implemented. The protocol crate can publish a complete route and OpenAPI contract without serializing secret material. Model-facing layers in Slice 3 receive ordinary JSON results and never the credential header; the adapter consumes and stores that header separately.
