# Progress

Current slice: 3 — CLI and cooperative MCP
Current gate: Slice 2 verified and merged; Slice 3 decomposed for execution
Last reconciled: 2026-08-08

## Verified

- Product and engineering PRD drafted.
- Autonomous build-loop controls drafted.
- W-000 repository foundation published at `spatial-bit/psst`.
- Cross-platform formatting, linting, and tests passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31209799830>.
- W-101 core domain model independently reviewed and verified: typed identities and values, state rules, durable idempotency semantics, stable errors, secret redaction, and 17 passing tests.
- W-101 cross-platform PR evidence passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31214329560>.
- W-102 SQLite foundation independently reviewed and locally verified: bundled SQLite, fail-closed embedded migrations, exact schema constraints, rollback/restart durability, and concurrent startup.
- W-102 cross-platform PR evidence passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31215449960>.
- W-103 squad and membership transactions independently reviewed and locally verified: durable lifecycle, immutable leave authority, roster projection, concurrency, rollback, restart, and stable errors.
- W-103 cross-platform PR evidence passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31216646126>.
- W-104 instance leases and resume independently reviewed and locally verified: exclusive ownership, deterministic heartbeat, 256-bit OS-generated credentials, atomic resume, restart, rollback, and non-disclosure.
- W-104 cross-platform PR evidence passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31217748580>.
- W-105 durable message submission independently reviewed and locally verified: persist-before-success, scoped idempotency, ambiguous-commit recovery, authorization, integrity metadata, concurrency, restart, and stable errors.
- W-105 cross-platform PR evidence passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31218581813>.
- W-106 inbox and acknowledgement independently reviewed and locally verified: cursorless replay, sequence order, bounded indexed reads, recipient-only atomic acknowledgement, restart, rollback, and corruption handling.
- W-106 cross-platform PR evidence passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31219378486>.
- W-107 Slice 1 reliability gate independently approved: 80 tests, repeated stress, real held-lock contention, mid-ack rollback, and abrupt-process commit-boundary evidence.
- W-107 revision `4c8e094` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31220050245>.
- Slice 1 merged to `main` at `fe4f0bf`; the merged revision passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31220421510>.
- W-201 protocol/OpenAPI contract independently reviewed and verified; GitHub Actions passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31223796106>.
- W-202 authenticated store boundary and relay runtime independently approved with 126 passing tests; revision `b99f9c4` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31227930335>.
- W-203 lifecycle HTTP API independently approved with 130 passing tests; revision `1d2a2ca` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31229928971>.
- W-204 message, inbox, acknowledgement, and transcript HTTP API independently approved with 137 passing tests; revision `7591018` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31231619550>.
- W-205 bounded long polling independently approved with 146 passing tests; revision `2273849` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31233081040>.
- W-206 typed Rust client independently approved with 158 passing tests: complete Slice 2 API coverage, memory-only redacted credentials, bounded transport, total operation deadlines, conservative retries, exact prepared-send idempotency, real ambiguous-commit recovery, and restart/offline/resume E2E evidence.
- W-206 revision `f1db97c` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31235067734>.
- W-207 Slice 2 reliability gate independently approved with 162 passing tests, 100 production TCP watchers, exact restart reconciliation, bounded disconnect/shutdown/lock recovery, and repeated release performance above 100 messages/second with p95 below 100 ms.
- W-207 revision `9bded29` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31237113554>.
- W-208 native development artifacts independently approved: revision-stamped relay-only archives, exact inventory/path scanning, bundled-SQLite inspection, native startup smoke, and clean downloaded rehearsal without Rust.
- W-208 revision `c3340fe` passed standard CI and native artifact builds on Windows x86-64, Linux x86-64, and macOS ARM64: <https://github.com/spatial-bit/psst/actions/runs/31238500463> and <https://github.com/spatial-bit/psst/actions/runs/31238500451>.
- Trusted upload and clean-download rehearsal retained three 14-day dogfood artifacts and passed on all three native platforms: <https://github.com/spatial-bit/psst/actions/runs/31238613310>.
- Slice 2 merged to `main` at `d8d6d88`; merged standard CI and native development-artifact workflows passed: <https://github.com/spatial-bit/psst/actions/runs/31239047533> and <https://github.com/spatial-bit/psst/actions/runs/31239047525>.
- W-301 Slice 3 contracts independently approved with 174 passing tests: complete CLI grammar and
  versioned output, exhaustive safe error and authority mapping, full closed MCP schemas, a pinned
  Rust 1.89-compatible SDK, and a Psst-owned bounded/redacting stdio transport with structured cleanup.
- W-301 revision `83fa79a` passed GitHub Actions on Windows, Linux, and macOS plus native Windows
  x86-64, Linux x86-64, and macOS ARM64 artifact builds: <https://github.com/spatial-bit/psst/actions/runs/31246729089>
  and <https://github.com/spatial-bit/psst/actions/runs/31246729075>.

## Active

- W-302 configuration, profiles, and credential store.
- W-303 cooperative session runtime is independently approved after aggregate adversarial review.
  Windows coordinator gates are green for the full workspace test suite, strict lint, and format;
  native Linux, macOS, and Windows CI remain pending.
- W-304 CLI shell and relay operations.
- W-306 MCP transport and schema contract.
- W-306 implementation passes exact-schema/runtime validation, independent stdio transcripts,
  strict workspace lint, and the 190-test Windows workspace gate; review and cross-platform CI remain pending.
- W-307 cooperative MCP business dispatch is implemented locally: all 11 MCP tests pass, including
  a real-relay two-child lifecycle/replay/ack/reconnect/injection/canary transcript and concurrent
  long-poll-versus-leave cancellation. Independent adversarial review approved the repaired owned
  mutation/publication, transition-aware shutdown, orphaned-leave startup topology, and guarded
  operation drain before profile-lock release. Strict full-workspace lint, formatting, and the
  serialized full-workspace Windows test gate pass; native CI remains pending.
- W-305 human squad and messaging CLI is independently approved locally after rebasing every
  protected operation onto W-303 and repairing failed-leave/archive shutdown paths. Native CI and
  the explicitly authorized ambiguous-commit proxy evidence remain pending.

## Ready

- W-308 cooperative dogfood artifacts and documentation are verified. The reviewed revision
  `c2e4dad3adf67b271a1b6a03a700711818e81503` passed all three native builds and all three
  checkoutless clean-download rehearsals in
  [workflow 31272964987](https://github.com/spatial-bit/psst/actions/runs/31272964987).
- W-309's automated reliability gate is independently approved and verified at
  `a4af73ad800dde8ceff8209768685e0d7cf19809`. Standard Windows/Ubuntu/macOS CI passed in
  [workflow 31274551562](https://github.com/spatial-bit/psst/actions/runs/31274551562); all three
  native reliability jobs and all three checkoutless artifact rehearsals passed in
  [workflow 31274817025](https://github.com/spatial-bit/psst/actions/runs/31274817025). The distinct
  live Claude Code/Codex walkthrough remains pending and is not implied by this automated evidence.

## Gate evidence

- W-301 Windows local formatting, strict lint, and 174-test workspace suite: passed
- W-301 independent contract and adversarial reviews: passed
- W-301 GitHub Actions matrix: passed on Windows, Linux, and macOS
- W-301 native artifact builds: passed on Windows x86-64, Linux x86-64, and macOS ARM64

## Next coordinator action

Complete W-309's live Claude Code/Codex walkthrough with distinct profiles at the exact candidate
revision. The loopback ambiguous-commit CLI proxy remains behind explicit user authorization. Alpha
tagging also remains blocked on owner-provisioned signing trust, protected-review configuration, an
isolated trusted-LAN rehearsal, and separate explicit tag/publication authorizations.
