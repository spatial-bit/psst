# Progress

Current slice: 3 — CLI and cooperative MCP
Current gate: Slice 3 automated gate verified; live cooperative walkthrough pending
Last reconciled: 2026-08-09

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
- W-302 configuration, native profile locking, and protected credential storage are independently
  approved. W-303's cooperative runtime is independently approved after aggregate adversarial
  review. W-304's CLI shell and relay operations, W-305's human squad/messaging CLI, W-306's MCP
  transport/schema, and W-307's cooperative MCP dispatch are independently approved.
- The integrated W-302 through W-307 candidate at
  `a4af73ad800dde8ceff8209768685e0d7cf19809` passed the complete workspace test, strict Clippy, and
  format gate on Windows, Ubuntu, and macOS in
  [workflow 31274551562](https://github.com/spatial-bit/psst/actions/runs/31274551562).

## Active

- The operator-directed `ae55f7b` live rehearsal passed bidirectional MCP send/replay/explicit-ack,
  linked reply, roster/heartbeat, and Claude restart/resume. It found Codex global-registration
  profile contention; the repair defers ownership until protected tool use.
- The lifecycle repair at `30c77d34c510e851f69d1418ff3eacc09b831cd2` passed all three standard
  native CI jobs and all three native cooperative artifact jobs. A post-repair Codex-only smoke then
  proved real MCP join/status, process exit, fresh-session resume/roster, and clean leave with the
  exact passing revision. The temporary registration and relay were removed afterward.
- PR #3 merged the complete Slice 3 candidate into `main` as
  `5bba7119aae9fdfacdefd47ce825efb74b5e590a`. Exact-merge three-OS CI passed in
  [workflow 31343911230](https://github.com/spatial-bit/psst/actions/runs/31343911230), and all three
  native cooperative builds plus clean-download rehearsals passed in
  [workflow 31343911214](https://github.com/spatial-bit/psst/actions/runs/31343911214). The fresh
  Windows alpha archive was also downloaded, extracted, and re-inspected locally.
- W-501 through W-503 release-contract, portable-asset, and checkoutless-rehearsal automation are
  implemented as unreleased candidates. A signed authorized tag, native exact-tag candidate run,
  owner-controlled isolated trusted-LAN rehearsal, and independent reviewer attestation remain
  pending. W-504 publication remains separately authorization-gated.
- The optional loopback CLI process-boundary ambiguity proxy remains deliberately unexecuted because
  forwarding ephemeral test credentials requires explicit user authorization. Approved
  client/runtime evidence already proves exact prepared-send retry below that process boundary.

## Ready

- W-308 cooperative dogfood artifacts and documentation are verified. The reviewed revision
  `c2e4dad3adf67b271a1b6a03a700711818e81503` passed all three native builds and all three
  checkoutless clean-download rehearsals in
  [workflow 31272964987](https://github.com/spatial-bit/psst/actions/runs/31272964987).
- W-309's automated reliability gate, operator-directed Claude/Codex rehearsal, post-repair Codex
  lifecycle smoke, and exact-merge native/clean-download verification are complete. Psst remains a
  cooperative tool: this evidence does not claim client launch, scheduling, or wake-on-mail.

## Gate evidence

- W-301 Windows local formatting, strict lint, and 174-test workspace suite: passed
- W-301 independent contract and adversarial reviews: passed
- W-301 GitHub Actions matrix: passed on Windows, Linux, and macOS
- W-301 native artifact builds: passed on Windows x86-64, Linux x86-64, and macOS ARM64

## Next coordinator action

Hand off the merged-main development archives for practical alpha use. The loopback ambiguous-commit
CLI proxy remains behind explicit user authorization. Formal signed-tag publication remains a
separate owner-authorized path; Slice 4 client activation and wake-on-mail are not part of this gate.
