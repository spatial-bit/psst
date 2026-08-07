# ADR 0001: Rust and bundled SQLite

Status: accepted

Psst uses Rust and `rusqlite` with bundled SQLite. This supports native cross-platform artifacts, a single-process relay, explicit concurrency boundaries, and durable local operation without a separately administered database.

The first implementation must verify Claude MCP Channel and Codex App Server integration from Rust before introducing another runtime boundary.

