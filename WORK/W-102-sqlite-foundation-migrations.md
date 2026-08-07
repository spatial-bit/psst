# W-102: SQLite foundation and migrations

Status: verified

## Objective

Create `psst-store` with bundled SQLite, deterministic connection policy, and forward-only embedded migrations that safely establish the Slice 1 schema.

## Requirement mapping

- NFR-002, NFR-005, NFR-007, NFR-009: durable bounded database operation.
- PRD §10: complete SQLite schema, indexes, foreign keys, WAL, busy timeout, synchronous policy, and migration rules.
- Testing §17: empty migration, historical upgrade, checksum mismatch, future schema, foreign-key, rollback, and restart coverage.

## Dependencies

- W-101.

## Allowed scope

- New `crates/psst-store/**`, including embedded migration files.
- Root workspace/dependency declarations required to add `psst-store` and bundled `rusqlite`.
- A narrowly scoped architecture decision only if implementation uncovers an undocumented SQLite durability tradeoff; otherwise escalate rather than editing outside scope.

No business operations beyond opening, migrating, inspecting, and closing a store.

## Acceptance

- SQLite is bundled; users do not need a separately installed SQLite library.
- Opening a new path creates the exact required tables, checks, foreign keys, partial indexes, and query indexes.
- Every connection enables foreign keys, bounded busy timeout, WAL, and a documented explicit synchronous policy.
- Ordered migrations are embedded, checksummed, and applied in an exclusive transaction.
- A checksum mismatch or database schema newer than the binary fails closed with a stable error.
- A failed migration rolls back without recording a version or leaving partial schema.
- Reopening a migrated database is idempotent and preserves rows.
- Store tests use real temporary SQLite files; mocks are not accepted as durability evidence.

## Tests and verification

- Inspect `PRAGMA foreign_keys`, `journal_mode`, `synchronous`, and busy timeout behavior.
- Test empty-to-current and at least one synthetic historical-to-current upgrade path.
- Test tampered checksum and future version rejection.
- Inject migration failure and verify atomic rollback.
- Close/reopen a temporary file and verify schema and seed data persist.
- Run `cargo test -p psst-store`, then all repository gates.

## Reviewer concerns

- Migration serialization must be real; process-local locking alone is insufficient.
- Tests must not silently use in-memory databases for restart claims.
- Avoid globally shared connections and unbounded retry-on-busy loops.
- Ensure timestamp and boolean representations match the PRD consistently.

## Verification evidence

- Independent review completed 2026-08-07; migration-ledger prefix validation, frozen historical-fixture evidence, and explicit schema-contract tests were added in response.
- `cargo fmt --check` passed on Windows.
- `cargo clippy --workspace --all-targets -- -D warnings` passed on Windows.
- `cargo test --workspace` passed on Windows: 17 core tests, 14 real-file store tests, and all doc tests.
- Store evidence covers PRAGMAs, STRICT schema, exact named indexes, frozen v1 upgrade, checksum/application/version/ledger corruption, atomic rollback, restart persistence, checks, composite cross-squad foreign keys, scoped dedupe uniqueness, and eight simultaneous openers across four new files.
