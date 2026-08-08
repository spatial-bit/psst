# W-203: Squad, membership, lease, and roster HTTP API

Status: verified

## Objective

Expose the verified squad, membership, instance, heartbeat, resume, archive, leave, and roster transactions through the versioned HTTP contract.

## Requirement mapping

- FR-001–FR-010 and FR-020–FR-024.
- NFR-001–NFR-005, NFR-009, NFR-010.
- PRD §11 lifecycle endpoints and Security §8.

## Dependencies

- W-202.

## Allowed scope

- Lifecycle/auth handlers in `psst-relay`, matching protocol DTOs, and real-relay HTTP tests.
- Narrow store API adapters only where necessary to call already verified transactions.

Do not implement messaging, inbox, transcript, long polling, token files, CLI, MCP, or activation.

## Acceptance

- List/create/describe/archive/join/leave/roster/heartbeat behavior matches the store lifecycle and stable wire errors.
- Joining a missing squad requires mission; archived squads reject mutation but retain reads.
- Instance authority is established from the adapter-controlled credential header and immutable IDs, never a routing name alone.
- Heartbeat and resume-token handling have no model-callable or ordinary response surface; any one-time credential bootstrap is explicitly isolated and redacted.
- Expired leases yield offline/unknown roster state without releasing membership identity.
- HTTP success is emitted only after each backing transaction commits.

## Tests and verification

- Full HTTP lifecycle against a temporary real SQLite file, including restart and valid resume.
- Concurrent join/name-claim through separate HTTP clients.
- Invalid/missing/expired/wrong-membership authorization matrix.
- Archive and leave race tests.
- Captured response/log/schema secret-disclosure tests.
- Run all repository gates.

## Reviewer concerns

- Preserve exact-retry and ownership semantics; handlers must not reimplement state rules.
- Map domain/store errors explicitly and never return SQL or internal error strings.
- Authentication lookup must be bounded and must not accept credentials in query strings.
- Clarify bootstrap credential delivery without allowing it into later MCP results.

## Verification evidence

- Independently reviewed and approved after closure of payload-limit mapping, canonical credential concealment, full lifecycle HTTP coverage, malformed request handling, and real bound-server concurrency/race findings.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` passed.
- `cargo test --workspace` passed with 130 tests: 17 core, 18 protocol, 22 relay, and 73 store tests, plus doc tests.
- Revision `1d2a2ca` passed GitHub Actions on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31229928971>.
