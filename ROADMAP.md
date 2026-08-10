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

Status: verified

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

Status: verified

Human CLI, safe token storage, MCP tools, and cooperative Claude/Codex workflows.

Execution order:

```text
W-301 Contracts and architecture
 ├─> W-302 Configuration, profiles, and credential store
 │    └─> W-303 Cooperative session runtime
 ├─> W-304 CLI shell and relay operations
 │    └──────────────┐
 │                   └─> W-305 Human squad and messaging CLI
 └─> W-306 MCP transport and schema contract
      └──────────────┐
                     └─> W-307 Cooperative MCP tools
W-305 + W-307 -> W-308 Dogfood artifacts and documentation
W-301 through W-308 -> W-309 Slice 3 cooperative gate
```

Gate: two independently launched interactive Claude Code and Codex agents exchange and acknowledge messages through the same relay using cooperative MCP tools; automated two-profile tests prove replay, restart, resume, heartbeat, and redaction on all supported platforms.

## Slice 4 — Harnessed activation

Status: verified

Claude MCP Channels, Codex App Server, wake coalescing, reconciliation, and bounded backoff.

Execution order:

```text
W-401 Activation contracts
  -> W-402 Client-neutral activation engine
      ├─> W-403 Claude Channel adapter
      └─> W-404 Codex App Server adapter
W-403 + W-404 -> W-405 Harness operations
W-401 through W-405 -> W-406 Slice 4 wake-on-mail gate
```

Gate: fake-host contract suites and native packaged rehearsals prove burst coalescing, dropped-wake reconciliation, bounded backoff, no active-turn preemption, and no secret or message-body injection; opt-in live Claude and Codex sessions each wake from durable pending mail and process it without duplicate or lost work.

## Slice 5 — Release engineering

Status: active

Native artifacts, checksums, SBOM, install/uninstall documentation, and cross-platform release rehearsal.

Execution order:

```text
W-501 Alpha release identity and contract
  -> W-502 Deterministic portable assets
      -> W-503 Checkoutless rehearsal and evidence bundle
          -> W-504 Separately authorized signed prerelease publication
```

Gate: the exact signed-tag revision passes native Windows x86-64, Linux x86-64, and macOS ARM64
builds; deterministic archive, manifest, checksum, SBOM, secret/path scan, install/uninstall, local
relay, replay/acknowledgement, restart, and MCP initialization checks pass from downloaded assets
without a checkout or Rust. Tagging, protected-environment configuration, isolated non-loopback LAN
operation, attestation, and publication remain explicit owner-controlled boundaries.
