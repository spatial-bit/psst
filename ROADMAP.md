# Roadmap

## Slice 0 — Repository and quality foundation

Status: verified

Gate: formatting, linting, and tests pass on Windows, Linux, and macOS; public documentation accurately claims no usable product behavior.

## Slice 1 — Core model and SQLite durability

Status: verified

Core types, migrations, transactions, squads, memberships, instances, messages, leases, idempotency, and acknowledgements.

Execution order:

```text
W-101 Core domain model
  -> W-102 SQLite foundation and migrations
      -> W-103 Squad and membership transactions
          -> W-104 Instance leases and resume
              -> W-105 Durable message submission
                  -> W-106 Inbox retrieval and acknowledgement
                      -> W-107 Slice 1 reliability gate
```

Gate: migration, restart persistence, idempotency, uniqueness, lease, replay, and acknowledgement tests pass against real temporary SQLite databases; workspace formatting, linting, and tests pass on Windows, Linux, and macOS.

## Slice 2 — Relay and typed client

Status: active

Versioned HTTP API, long polling, structured errors, shutdown, health, and typed Rust client.

Execution order:

```text
W-201 Wire contract and OpenAPI baseline
  -> W-202 Relay runtime, configuration, health, and store isolation
      -> W-203 Squad, membership, lease, and roster HTTP API
          -> W-204 Durable messaging and transcript HTTP API
              -> W-205 Bounded long polling and post-commit notification
                  -> W-206 Typed Rust client and retry boundaries
                      -> W-207 Relay/client reliability and shutdown gate
                          -> W-208 Cross-platform development artifacts
```

Gate: a real relay and typed client pass restart, offline delivery, replay, concurrent watcher, timeout, cancellation, and shutdown tests on Windows, Linux, and macOS. Native CI development artifacts are retained for Slice 3 dogfooding; signed tags, checksums, SBOMs, reproducible release archives, and GitHub Release publication remain Slice 5 scope.

## Slice 3 — CLI and cooperative MCP

Human CLI, safe token storage, MCP tools, and cooperative Claude/Codex workflows.

## Slice 4 — Harnessed activation

Claude MCP Channels, Codex App Server, wake coalescing, reconciliation, and bounded backoff.

## Slice 5 — Release engineering

Native artifacts, checksums, SBOM, install/uninstall documentation, and cross-platform release rehearsal.
