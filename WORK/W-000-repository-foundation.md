# W-000: Repository foundation

Status: active

## Objective

Establish a public MIT-licensed Rust workspace with durable engineering controls and a minimal cross-platform CI gate.

## Acceptance

- Repository documents do not claim usable product behavior.
- `cargo fmt --check` passes.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- `cargo test --workspace` passes.
- CI runs those checks on Windows, Linux, and macOS.
- No local database, secret, or build output is tracked.
- The initial public repository is `spatial-bit/psst` unless the owner decision changes.

