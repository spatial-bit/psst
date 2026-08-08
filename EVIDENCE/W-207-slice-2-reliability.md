# W-207 Slice 2 reliability evidence

Reviewed revision: `9bded29`

## Functional gate

- `cargo test --workspace --all-targets --all-features`: 162 passed, 0 failed, 0 ignored.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed.
- GitHub Actions passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31237113554>.
- Named public-path suite: `crates/psst-client/tests/slice2_reliability.rs`.

The suite uses the production relay listener and a real SQLite file. It proves 100 simultaneously registered TCP long polls, exact targeted delivery, registration cleanup, subsequent admission reuse, real client disconnect cleanup, bounded production shutdown, listener refusal, database reopening, external writer-lock recovery, and exact prepared-send idempotence.

## Release performance gate

Environment: Windows x86-64 developer machine, 12 logical CPUs, loopback TCP, release profile, bundled SQLite, `journal_mode=WAL`, `synchronous=FULL`, WAL auto-checkpoint at 256 pages, 16-request maximum send concurrency, 105 offered messages/second, 30 seconds per repetition.

Three independently reviewed repetitions completed 9,451 durable sends with no errors, loss, or duplicate logical messages:

| Run | Completed rate | Send p95 | Send p99 | Send max | Inbox `wait=0` p95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 105.0 msg/s | 8.165 ms | 15.750 ms | 38.334 ms | 1.363 ms |
| 2 | 105.0 msg/s | 12.134 ms | 18.360 ms | 36.521 ms | 1.600 ms |
| 3 | 105.0 msg/s | 11.020 ms | 17.945 ms | 31.231 ms | 1.384 ms |

Every repetition captured the returned message identities and reconciled exact IDs, bodies, and contiguous sequences against transcript and inbox state before and after a graceful same-file relay restart. Adapter-owned heartbeats renewed both sessions during each long run.

The initial 1,000-page WAL auto-checkpoint policy produced a reproducible accumulated-state send p95 spike as high as 517 ms. Reducing the checkpoint interval to 256 pages retained `synchronous=FULL` durability while replacing the serialized large checkpoint with smaller, more frequent checkpoints. The repeated results above include many checkpoint cycles. The tradeoff is potentially greater checkpoint/write activity.

## Repetition and review

- The focused shutdown/disconnect test passed 10 consecutive runs.
- The Windows deadline regression passed 10 consecutive focused runs after unrelated session bootstrap was isolated from its deliberate 25 ms test deadline.
- Independent review rejected two earlier iterations for false or incomplete proofs, then approved the final exact-accounting, bounded-shutdown, repeated-tail revision.
- No credentials, message-body canaries, database files, listeners, or relay processes are retained by this evidence artifact.
