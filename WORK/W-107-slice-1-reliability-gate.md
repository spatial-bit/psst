# W-107: Slice 1 reliability and evidence gate

Status: verified

## Objective

Independently validate the complete core/store slice under restart, concurrency, lock contention, and injected transaction failures, then produce truthful gate evidence without adding product scope.

## Requirement mapping

- All Slice 1 mappings from W-101 through W-106.
- NFR-001–NFR-007 and NFR-009–NFR-010.
- Testing §17 store, migration, concurrency, and relevant fault-injection requirements.
- Slice 1 gate in PRD §20.

## Dependencies

- W-101 through W-106 implementation complete and separately reviewed.

## Allowed scope

- `crates/psst-core/**` and `crates/psst-store/**` tests and defect fixes only.
- `tests/**` for cross-crate Slice 1 tests if a public interface warrants them.
- `WORK/W-101` through `WORK/W-107` evidence sections and `PROGRESS.md` only when verification is complete.

No relay, HTTP, client, CLI, MCP, activation, packaging, or feature expansion.

## Acceptance

- Every Slice 1 requirement has a trace to an automated test or a documented reason it belongs to a later slice.
- Real-file SQLite scenarios cover migration, rollback, restart, offline delivery, retry after ambiguous commit, retrieval without acknowledgement, crash before/after acknowledgement, simultaneous name claim, lease expiry/resume, and database busy behavior.
- Failure injection demonstrates no partial multi-row operation for create/join, resume, send, or acknowledgement.
- Tests are deterministic: no timing sleeps, network dependency, shared user state, or order dependency.
- Full workspace formatting, linting, and tests pass locally.
- Windows, Linux, and macOS CI pass on the reviewed revision before Slice 1 is marked verified.

## Tests and verification

- Run `cargo fmt --check`.
- Run `cargo clippy --workspace --all-targets -- -D warnings`.
- Run `cargo test --workspace`.
- Run any ignored fault suite explicitly; there should be no unexplained skipped tests.
- Record revision, exact commands, outcomes, and cross-platform CI URL in work-unit evidence and `PROGRESS.md`.

## Reviewer concerns

- Reviewer must be independent of the primary implementation assignments.
- Green tests are insufficient if they mock durability or omit concurrency.
- Do not weaken `synchronous`, busy timeout, constraints, or assertions to remove failures.
- Any data-loss, token-exposure, cross-squad, or acknowledgement-authority defect blocks the gate.

## Verification evidence

- Independently audited and approved at revision `4c8e094` on 2026-08-07.
- `cargo fmt --check` passed on Windows.
- `cargo clippy --workspace --all-targets -- -D warnings` passed on Windows.
- `cargo test --workspace` passed: 17 core tests, 63 store tests, 0 failed, 0 ignored, and all doc tests.
- Repeated stress passed: 20 contention runs, 20 mid-ack rollback/reopen runs, 10 abrupt-process acknowledgement-boundary runs, plus the prior 100 concurrency/race/rollback executions.
- Real-file evidence covers held-lock `database_busy` and recovery, mid-batch mutation rollback, abrupt process death before and after committed acknowledgement, migrations, restart, offline delivery, ambiguous commit, replay, name claims, expiry/resume, and bounded indexed inbox work.
- GitHub Actions passed on Windows, Linux, and macOS for revision `4c8e094`: <https://github.com/spatial-bit/psst/actions/runs/31220050245>.
