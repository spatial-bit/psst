# W-303: Cooperative session runtime

Status: blocked on W-302

## Objective

Provide the single adapter-owned lifecycle for profile locking, session validation, heartbeat, explicit lease-expiry resume, credential rotation, and bounded shutdown.

## Dependencies

- W-302.

## Acceptance

- Startup loads and exclusively locks one profile, heartbeats immediately, and publishes readiness only after session validation and any replacement credential is durable.
- Heartbeats are non-overlapping, follow relay-advertised cadence, use monotonic scheduling, and never translate unknown availability to idle.
- Only explicit lease expiry triggers serialized resume; transport and semantic ambiguity enter a sanitized degraded/outcome-unknown state without inventing identity.
- Credential snapshots used by tool/command calls remain consistent across rotation.
- Cancellation bounds heartbeat/backoff/long-poll shutdown; leave stops heartbeat first and clears local authority only after confirmed success.
- Fake-clock, restart, expiry, rotation fault, lock-contention, cancellation, and credential-canary tests pass.

## Verification evidence

Pending.
