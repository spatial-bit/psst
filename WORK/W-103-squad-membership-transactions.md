# W-103: Squad and membership transactions

Status: verified

## Objective

Implement durable squad creation, listing, description, archive, join, roster, and leave operations with atomic membership-name ownership.

## Requirement mapping

- FR-001–FR-006, FR-008–FR-010.
- NFR-001–NFR-003, NFR-009–NFR-010.
- PRD §9 squad/membership lifecycles and §10 atomic create/join requirement.

## Dependencies

- W-102.

## Allowed scope

- `crates/psst-core/**` only for omissions exposed by implementing the approved domain contract.
- `crates/psst-store/**` squad and membership repository operations and tests.

Do not implement resume, heartbeat, messaging, HTTP, or CLI behavior.

## Acceptance

- Squad creation requires a unique valid name and non-empty mission.
- Join can atomically create a missing squad only when a mission is supplied.
- Two simultaneous joins cannot establish the same active membership name in one squad.
- The same name may be used in different squads; a left membership retains history.
- Archive is irreversible and rejects subsequent joins while preserving reads.
- Leave closes the active instance if present and marks membership left in one transaction.
- Roster data distinguishes durable membership from current transport presence; unknown availability is never returned as idle.
- Failure injection at each multi-write boundary leaves no partial squad/agent/membership state.

## Tests and verification

- Real-file SQLite tests for create/list/describe/archive and restart persistence.
- Concurrent name-claim test using independent connections; exactly one claimant succeeds.
- Atomic implicit-create-and-join rollback test.
- Cross-squad same-name and post-leave history tests.
- Archived-squad rejection tests.
- Run focused store tests, then all repository gates.

## Reviewer concerns

- Do not rely only on preflight queries for uniqueness; database constraints/transactions must close races.
- Reads must not mutate lease state.
- Joining must not overwrite an existing mission or revive an archived squad.
- Errors must preserve stable domain codes without exposing SQL text.

## Verification evidence

- Independent review completed 2026-08-07; leave authorization was hardened from reusable routing name to immutable `(SquadId, MembershipId)` identity before approval.
- Roster projection now includes mode and a validated availability observation; offline or absent transport is always `unknown/unknown` at the observation time.
- `cargo fmt --check` passed on Windows.
- `cargo clippy --workspace --all-targets -- -D warnings` passed on Windows.
- `cargo test --workspace` passed on Windows: 17 core tests, 28 store tests, and all doc tests.
- Evidence includes independent-connection name claims, archive/join serialization, restart persistence, every multi-write rollback boundary, stale-owner/name-reuse safety, archived reads, cross-squad name reuse, and sanitized stable errors.
