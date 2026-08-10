# W-401: Slice 4 activation contract

Status: active

## Objective

Freeze the client-neutral wake contract, state machine, adapter boundary, configuration, diagnostics, and hostile-content rules before adding host activation.

## Requirement mapping

- FR-050–FR-060, NFR-003–NFR-007.
- PRD §§9, 13–15 and the Slice 4 gate in §20.
- ADR 0005.

## Dependencies

- Slice 3 merged and verified on `main` at `ef64224b733e3ead232d95052fe9f1b62c2eb63c`.

## Allowed scope

- ADR, work-unit decomposition, closed activation DTOs, error taxonomy, configuration contract, fake-host interface, and deterministic contract tests.
- Dependency/MSRV spikes against installed-version Claude and Codex protocol surfaces.

Do not launch a client, emit a Channel notification, call App Server, add a public listener, or acknowledge mail.

## Acceptance

- The state transitions `quiet -> pending -> waking -> running`, bounded `backoff`, and fail-closed `blocked` are explicit and exhaustive.
- One profile has at most one outstanding wake; active turns are never interrupted for ordinary mail.
- Wake inputs are fixed instructions plus bounded trusted metadata and mechanically exclude message bodies and secret-shaped fields.
- Inbox truth, acknowledgement ownership, startup/post-turn/periodic reconciliation, retry classification, jitter bounds, shutdown, and diagnostic redaction are explicit.
- Claude Channels and Codex App Server version/capability boundaries are recorded from primary documentation without importing either protocol into `psst-core` or the relay.
- Slice 4 is decomposed into dependency-ordered work units with exact gates.

## Verification evidence

Pending implementation and native CI.
