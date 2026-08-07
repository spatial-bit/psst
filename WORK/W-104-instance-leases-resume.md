# W-104: Instance leases, heartbeat, and resume

Status: verified

## Objective

Implement single-owner instance claims, adapter-owned lease renewal, expiry-derived presence, and token-based resume as atomic store operations.

## Requirement mapping

- FR-005–FR-007, FR-020–FR-024.
- NFR-002–NFR-005, NFR-009–NFR-010.
- Security §8 token entropy, hashing, and non-disclosure requirements.
- PRD §9 instance lifecycle and §10 atomic resume/predecessor closure.

## Dependencies

- W-103.

## Allowed scope

- `crates/psst-core/**` for approved lease/token domain boundaries.
- `crates/psst-store/**` instance, heartbeat, presence, and resume operations/tests.

No background heartbeat loop, token file storage, HTTP authorization, or adapter implementation.

## Acceptance

- Join/claim returns the configured heartbeat interval and lease duration, defaulting to 10 and 30 seconds.
- Only one live instance may own a membership at a time, including under concurrent claims.
- Heartbeat extends only the authenticated current instance lease and updates last-seen deterministically.
- Lease expiry changes computed transport presence to offline without altering membership history.
- Resume validates a high-entropy opaque token by its stored hash, closes any stale predecessor, and creates a new instance atomically.
- Raw resume tokens never persist in SQLite and never appear in debug/error output.
- Time-dependent tests use an injected clock rather than sleeps.

## Tests and verification

- Concurrent instance-claim test with independent connections.
- Boundary tests immediately before, at, and after lease expiry.
- Valid resume, invalid token, live-owner conflict, and atomic rollback tests.
- Database inspection test verifies only token hashes are stored.
- Restart test proves valid resume after closing and reopening the store.
- Run focused store/core tests, then all repository gates.

## Reviewer concerns

- Constant-time comparison is preferred where the chosen hash API supports it; no plaintext fallback.
- Wall-clock movement must not accidentally extend leases; injected UTC time semantics must be documented.
- Resume must create a new instance ID, not reactivate a stale row.
- Model-facing types must have no path to token material.

## Verification evidence

- Independent review completed 2026-08-07; approval followed remediation of token entropy and availability/source write-boundary findings.
- Initial claim generates a canonical 256-bit token from the OS CSPRNG and exposes it only through a non-debuggable, non-serializable adapter outcome; SQLite stores only a domain-separated SHA-256 hash and verification is constant-time.
- `cargo fmt --check` passed on Windows.
- `cargo clippy --workspace --all-targets -- -D warnings` passed on Windows.
- `cargo test --workspace` passed on Windows: 17 core tests, 37 store tests, and all doc tests.
- Evidence includes concurrent ownership, exact lease boundaries, clock rollback, invalid/live/closed resume paths, atomic rollback, restart continuity, token inspection, leave interaction, and pre-transaction rejection of invalid availability observations.
