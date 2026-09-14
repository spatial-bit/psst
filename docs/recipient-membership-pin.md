# Selecting a stable recipient

`POST /v1/messages` accepts an optional `recipient_membership_id` alongside the
recipient name. A consumer that selects recipients from a roster should persist
the membership ID with each selected name and include it when eventually sending.
The relay checks that identity within the same transaction that resolves the name
and commits the message. A new send with a mismatched identity fails with
`recipient_not_found`; it is never redirected to a replacement using the same name.

An exact committed retry returns its original delivery even if that member later
left. A retry that supplies a different membership pin fails with
`idempotency_conflict`. The existing stored recipient identity already provides
this evidence, so no database migration is needed. Without a pin, current by-name
semantics remain unchanged. Older servers reject the unknown field rather than
silently accepting an unpinned request. Rust consumers constructing request structs
must initialize the new optional field; `Client::send` retains its current behavior.

This is a prerequisite for a consumer's durable one/some/all recipient selection,
not a broadcast implementation, execution grant, membership authorization change,
or evidence that any recipient handled the message. Each eventual recipient still
needs its own outcome and correlation record. Selecting names again after a failure
is not equivalent to retrying the original selection.

Validation includes real-store leave/name reuse and committed replay across reopen,
and a real local HTTP relay/typed-client test. Cross-platform evidence remains CI's
responsibility; local macOS tests do not establish Windows/Linux execution.
