# W-303: Cooperative session runtime

Status: implemented and independently approved; cross-platform CI pending

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

- Real-relay lifecycle test proves durable join publication, immediate validation heartbeat,
  profile-lock contention, explicit availability heartbeat, metadata-fault credential scrub, and
  confirmed leave cleanup.
- Injected manual-clock and scripted-transport evidence proves advertised cadence changes,
  non-overlap, exact 1/2/4-second exponential backoff, and no request before the clock fires.
- Explicit `LeaseExpired` alone performs one serialized resume. Successful rotation is persisted
  before its generation/instance snapshot is visible; injected persistence failure retains the
  prior generation/instance and reports `RotationFailed`.
- `OutcomeUnknown` and ordinary relay failure do not resume or invent identity and publish their
  distinct safe health states.
- Cancellation interrupts cadence sleep, backoff, a blocked heartbeat, and a blocked resume under
  an external bound; once resume issuance begins it instead completes and persists before exit.
- Dropping a blocked join waiter leaves its owned transaction and profile lock alive until issuance
  finishes; a competing lock is rejected throughout. Runtime drop aborts scheduled heartbeat before
  releasing the lock, and firing the abandoned cadence produces no traffic.
- Real inbox long polling is independently cancellable without the lifecycle gate. Read epochs and
  mutation gates prevent roster, inbox, transcript, acknowledgement, or archive dispatch after
  shutdown or terminal leave.
- Join, startup, resume, and confirmed leave cleanup run in owned lock-retaining transactions.
  Cancellation cannot abandon issued identity mutation; ambiguous leave retains authority, metadata,
  heartbeat ownership, and a restartable runtime. Immediate availability heartbeat routes explicit
  lease expiry through the same durable resume transaction as scheduled heartbeat.
- Windows profile removal validates and deletes the exact held no-reparse object; replacement and
  pathname deletion remain blocked until disposition completes.
- Recursive canary scanning finds the exact raw authorization value only in the restricted
  credential record.
- Leave Intent/Confirmed journaling is crash-recoverable and idempotent across relay ambiguity and
  every local cleanup boundary. A real relay commit followed by abrupt process termination is
  reconciled on restart without inventing or losing authority.
- Owned send operations retain one prepared dedupe identity after caller cancellation, are bounded
  by actual request and terminal-response memory, and drain before leave. Hostile or mismatched
  relay responses are rejected before retention or authority mutation.
- Client origin must exactly match the canonical bound profile origin before any authenticated
  request. Join, resume, and leave responses are checked against the requested or bound squad,
  member, role, and membership before durable publication or cleanup.
- All public client requests validate before network use; relay responses that affect authority,
  scheduling, messaging, or retained memory receive bounded semantic validation.
- Fresh aggregate adversarial review approved the combined C1 journal, C2 lifecycle recovery, C3
  send ledger, cancellation topology, validation boundaries, and cross-checkpoint interactions.
- Coordinator Windows gate: `cargo test --workspace --all-targets --all-features --locked --
  --test-threads=1` passed. Application evidence was 35 passed with 3 intentional subprocess crash
  fixtures ignored directly and exercised by active parent tests; contract and profile-lock suites
  also passed.
- Coordinator strict `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
  and `cargo fmt --all -- --check`: passed.
- Successful shutdown releases the profile guard only after reports, owned sends, scheduler, and
  the lifecycle operation gate have drained. A deterministic blocked-leave race denies a second
  owner until terminal cleanup, then proves a new owner's sentinel survives with no stale cleanup.
- Native Linux, macOS, and Windows CI remains authoritative and pending.
