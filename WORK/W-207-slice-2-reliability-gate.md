# W-207: Slice 2 relay/client reliability and shutdown gate

Status: active

## Objective

Independently validate the complete relay and typed-client slice under restart, concurrency, cancellation, timeout, contention, and shutdown without expanding into Slice 3 product surfaces.

## Requirement mapping

- All Slice 2 mappings from W-201 through W-206.
- NFR-001–NFR-010 applicable to the relay/client path.
- PRD §17 integration/concurrency and cross-platform E2E requirements.
- Slice 2 gate in PRD §20.

## Dependencies

- W-201 through W-206 implementation complete and separately reviewed.

## Allowed scope

- Relay/client/protocol/store tests and defect fixes only.
- `tests/contracts/**` and `tests/e2e/**` for public-path evidence.
- Work-unit evidence and `PROGRESS.md` after verification.

No CLI, token persistence, MCP, client scheduler, LAN discovery, harness activation, or release publication.

## Acceptance

- A typed client drives a real relay and real SQLite file through join, roster, offline send, receive, replay, acknowledge, restart, and resume.
- Long-poll wake, timeout, cancellation, irrelevant notifications, lost notifications, and relay shutdown are deterministic and leak-free.
- One hundred concurrent watchers and a sustained 100 messages/second test complete without data loss; local non-waiting API p95 is measured against the PRD target without hiding environment details.
- Held database locks, saturated internal queues, malformed requests, client disconnects, and ambiguous commits produce stable bounded behavior.
- Cross-platform CI runs at least one real relay/client E2E test on Windows, Linux, and macOS.

## Tests and verification

- Run formatting, clippy with warnings denied, and all workspace tests.
- Run focused stress/fault suites repeatedly and record counts, failures, timings, revision, and environment.
- Capture restart/offline/replay transcript and secret/log scan evidence.
- Require independent review before marking verified.
- Record the cross-platform GitHub Actions URL for the reviewed revision.

## Reviewer concerns

- Do not substitute mocks for the Slice 2 durability and restart gate.
- Performance claims must identify build profile, hardware/runner, dataset, and measurement method.
- No disabled/ignored tests or weakened timeout/durability settings to obtain green.
- Ordinary completion must leave no relay process, listener, or temporary credential behind.

## Verification evidence

Pending.
