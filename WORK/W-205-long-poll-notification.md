# W-205: Bounded long polling and post-commit notification

Status: verified

## Objective

Add cancellation-safe inbox long polling with post-commit local notification as an optimization while retaining SQLite as the only source of truth.

## Requirement mapping

- FR-036, FR-039, FR-041, FR-043, FR-050–FR-053 only for relay-side pending detection metadata.
- NFR-004–NFR-007.
- PRD §11 long-poll requirements and §17 concurrency tests.

## Dependencies

- W-204.

## Allowed scope

- Relay-local per-recipient notification registry, inbox wait handler, shutdown integration, and tests.
- Post-commit notification hook at the relay/store boundary.

Do not implement adapter wake state machines, MCP Channels, Codex activation, background heartbeat, or message-body notifications.

## Acceptance

- `wait` is bounded from 0 through 30 seconds; pending mail returns immediately and timeout returns `200` with an empty list.
- The flow checks durable pending state before sleeping and after every notification so missed/coalesced signals cannot hide mail.
- Send wakes only the relevant local recipient waiters and only after the transaction commits.
- Multiple messages may coalesce; priority metadata never changes authoritative inbox order.
- Client disconnect, request cancellation, timeout, and relay shutdown promptly release registrations and tasks.
- At least 100 concurrent watchers are bounded, isolated from commits, and do not create an unbounded task/channel/resource leak.

## Tests and verification

- Deterministic wake, preexisting-mail, timeout, dropped-notification reconciliation, and post-commit ordering tests.
- Cancellation before and after registration plus relay-shutdown cancellation tests.
- One hundred concurrent watchers with relevant and irrelevant recipients.
- Slow/disconnected watcher test proving unrelated message commits proceed.
- Restart test proving correctness without in-memory notification state.
- Run all repository gates.

## Reviewer concerns

- Eliminate check-then-sleep lost-wake races with a documented sequence.
- Registry cleanup must occur on every exit path.
- Notification payloads contain no message bodies or credentials.
- Do not hold SQLite transactions or connections while awaiting network events.

## Verification evidence

- Independently reviewed and approved after no-lost-wake, post-commit notification, sender cancellation, failed-send isolation, capacity cleanup, shutdown, and exact deadline findings were closed.
- `cargo fmt --check`, strict all-target/all-feature Clippy, and `git diff --check` passed.
- `cargo test --workspace` passed with 146 tests: 17 core, 18 protocol, 38 relay, and 73 store tests, plus doc tests.
- Revision `2273849` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31233081040>.
