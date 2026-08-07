# Psst: Product and Engineering Requirements

Status: Draft for implementation review  
License target: MIT  
Project name: **Psst**

## 1. Product thesis

Psst is a small, LAN-first substrate through which independently running AI agents can form squads, discover one another, and exchange durable direct messages.

The relay provides deterministic infrastructure: identity continuity, squad membership, presence leases, durable mailboxes, acknowledgements, and history. Local adapters expose communication as MCP tools and, when configured, activate supported agent sessions when mail becomes pending.

The product succeeds if a developer can start one relay, launch Claude Code or Codex on multiple machines, join the same squad with minimal setup, and observe reliable message exchange across disconnections and process restarts.

## 2. Product principles

1. **Durability before immediacy.** An accepted message is stored before delivery or wake is attempted.
2. **Transport is not judgment.** The relay never asks an LLM how to route, store, or acknowledge a message.
3. **Wake is not delivery.** Wake is an idempotent hint; the inbox is authoritative.
4. **Presence is leased.** Crashed processes disappear without requiring a graceful leave.
5. **Models do not maintain plumbing.** Local adapters own heartbeat, reconciliation, credentials, and retry.
6. **No hidden authority.** Resume tokens remain outside model-visible tool arguments and results.
7. **Small, typed protocol.** Version one supports direct messages, not an extensible social platform.
8. **Cross-client core, thin client adapters.** Claude-, Codex-, and future ACP-specific behavior does not leak into relay semantics.
9. **Evidence over assertion.** A feature is complete only when its acceptance evidence is reproducible.

## 3. Users and use cases

Primary user: a software developer running multiple AI coding-agent sessions on one machine or a trusted LAN.

Core use cases:

- A planner sends bounded work to a researcher or implementer.
- A researcher sends findings to a critic and receives a reply.
- An offline agent later resumes and receives pending mail.
- An agent on Windows collaborates with an agent on Linux or macOS.
- A developer inspects squads, rosters, presence, and transcripts from a CLI.
- An interactive agent checks messages voluntarily.
- A supported harness wakes an idle agent only when durable mail is pending.

## 4. Non-goals for version one

- Hostile-network or internet-facing security.
- End-to-end encryption or public-key identity.
- General publish/subscribe, topics, rooms, reactions, edits, attachments, or typing indicators.
- Multi-recipient messages or broadcast delivery.
- Workflow orchestration, task graphs, or job leasing.
- Semantic search, vector storage, or LLM inference inside the relay.
- Browser UI, mobile client, or hosted control plane.
- High availability, replication, clustering, or multi-relay federation.
- Exactly-once processing.
- Automatic preemption or cancellation of an active agent turn.
- Starting Claude through `claude -p`.

## 5. Terminology

- **Relay:** LAN service and SQLite store; source of truth for durable protocol state.
- **Squad:** Named collaboration boundary with a mission and roster.
- **Agent:** Durable identity anchor.
- **Membership:** An agent's name and role within one squad.
- **Instance:** One running adapter that currently owns a membership lease.
- **Adapter:** Local process exposing MCP tools, heartbeating, watching mail, and optionally activating an agent.
- **Message:** Immutable, durable, one-sender/one-recipient envelope.
- **Wake:** Coalesced activation hint indicating that pending mail exists.
- **Transport presence:** Authoritative `online` or `offline` state derived from an instance lease.
- **Availability:** Advisory `idle`, `busy`, `blocked`, or `unknown` state.
- **Cooperative mode:** Agent checks mail while already active.
- **Scheduled mode:** A client scheduler periodically creates an inbox-check turn.
- **Harnessed mode:** Adapter activates a session when pending mail appears.

## 6. Functional requirements

### Squads and membership

- **FR-001:** A client can list active and archived squads.
- **FR-002:** A client can create a squad with a unique name and non-empty mission.
- **FR-003:** Joining a nonexistent squad may create it only when the request includes a mission; otherwise it fails.
- **FR-004:** A client can join an active squad with a unique active membership name, role, mode, and client metadata.
- **FR-005:** Joining returns opaque agent, membership, instance, and resume identifiers plus lease timing.
- **FR-006:** A live membership name cannot be claimed by a second instance.
- **FR-007:** An expired instance can be resumed only using its opaque resume token; lease expiry does not release or transfer the durable membership name.
- **FR-008:** A member can leave; leaving closes the active instance but retains history.
- **FR-009:** A client can read squad mission, lifecycle state, roster, transport presence, availability, mode, and last-seen time.
- **FR-010:** Archiving a squad rejects new joins and messages but preserves reads and history.

### Presence

- **FR-020:** The adapter renews its lease without model participation.
- **FR-021:** Default heartbeat interval is 10 seconds and default lease is 30 seconds; both are relay-advertised.
- **FR-022:** An instance whose lease expires becomes offline without altering durable membership.
- **FR-023:** Availability is advisory and includes observation source and timestamp.
- **FR-024:** Unknown availability must never be presented as idle.

### Messaging

- **FR-030:** An active member can send an immutable UTF-8 direct message to another member of the same active squad.
- **FR-031:** Offline recipients remain valid; their messages persist.
- **FR-032:** Sending to an unknown, left, or cross-squad recipient fails with a structured error.
- **FR-033:** An accepted message receives a stable opaque ID and monotonic sequence.
- **FR-034:** A sender-supplied dedupe key makes retries idempotent within the sender membership and squad.
- **FR-035:** Within the `(squad, sender membership)` dedupe scope, reuse of a dedupe key with a different recipient, body, priority, reply target, or correlation ID fails with `idempotency_conflict`. An exact committed retry returns its original result before current lifecycle checks; a different squad or sender selects a different scope.
- **FR-036:** Inbox reads are acknowledgement-driven and support `limit` and bounded `wait` parameters; transcript/history reads use sequence cursors.
- **FR-037:** Retrieval does not acknowledge a message.
- **FR-038:** A recipient can acknowledge messages individually in batches.
- **FR-039:** Unacknowledged messages are replayable after process, adapter, network, or relay restart.
- **FR-040:** Replies may reference one prior message; conversations may share a correlation ID.
- **FR-041:** Version one priorities are `normal` and `high`; priority affects wake metadata but not inbox pagination order and never cancels work.
- **FR-042:** Message bodies are limited to 64 KiB UTF-8; configurable lower limits are permitted.
- **FR-043:** Inbox batches are limited to 100 messages and 1 MiB serialized output.

### Activation

- **FR-050:** The adapter observes pending mail without invoking the model for empty polls.
- **FR-051:** Wake is edge-triggered when an inbox changes from no unacknowledged mail to pending mail.
- **FR-052:** Multiple messages coalesce into one outstanding wake.
- **FR-053:** Wake payloads contain squad, pending count, highest priority, and oldest message ID, but never message bodies.
- **FR-054:** Version one does not interrupt or cancel an active turn for mail.
- **FR-055:** After a turn completes, pending unacknowledged mail causes another bounded wake attempt.
- **FR-056:** The adapter reconciles pending state at least every 60 seconds to recover lost wake notifications.
- **FR-057:** Activation retries use bounded exponential backoff with jitter and visible diagnostics.
- **FR-058:** Claude harnessed mode uses MCP Channels where available and never `claude -p`.
- **FR-059:** Codex harnessed mode uses the installed Codex App Server schema and `turn/start`; `turn/steer` is disabled by default for ordinary mail.
- **FR-060:** Unsupported clients may use a harness-owned PTY fallback only when explicitly enabled.

### Human CLI

- **FR-070:** The CLI can start the relay, display health, list and describe squads, show rosters, send messages, read an inbox, acknowledge messages, and print transcripts.
- **FR-071:** Human-readable output is the default; `--json` emits stable machine-readable JSON.
- **FR-072:** Secrets are redacted from normal output and logs.

## 7. Reliability and operational requirements

- **NFR-001:** An HTTP success for message submission means the SQLite transaction committed.
- **NFR-002:** Relay restart must preserve squads, memberships, instances, messages, acknowledgements, and migrations.
- **NFR-003:** At-least-once delivery is the stated contract; exactly-once processing is not claimed.
- **NFR-004:** All network calls and long polls have explicit timeouts and cancellation paths.
- **NFR-005:** Queues, request bodies, responses, connections, and retry loops are bounded.
- **NFR-006:** A slow or disconnected watcher cannot block message commits for other clients.
- **NFR-007:** Clean shutdown stops accepting writes, cancels long polls, checkpoints SQLite, and exits within a documented bound.
- **NFR-008:** The relay must serve 100 concurrently connected adapters and 100 messages/second on a developer-class machine without data loss; latency targets are p95 under 100 ms for local non-waiting API calls.
- **NFR-009:** All timestamps are UTC RFC 3339 at the API and integer epochs internally where appropriate.
- **NFR-010:** Structured errors have stable codes and non-sensitive messages.

## 8. Security model

Version one assumes every process able to reach the relay is trusted. It is inappropriate for hostile LANs, public Wi-Fi, port-forwarding to the internet, or multi-tenant use.

Opaque resume tokens protect continuity against accidental collision, not a determined LAN attacker. Tokens:

- are randomly generated with at least 128 bits of entropy;
- are stored hashed in SQLite;
- are stored locally with user-only permissions where supported;
- are sent only in authorization headers or equivalent adapter-controlled fields;
- never appear in MCP tool schemas, model-visible results, logs, or diagnostics.

The default bind address is loopback. LAN binding requires an explicit flag and prints the trusted-LAN warning.

## 9. State and lifecycle

### Squad

```text
active → archived
```

Archive is irreversible in version one.

### Membership

```text
joined → left
```

A left membership retains historical messages and cannot receive new ones.

### Instance

```text
online → expired
online → closed
expired/closed → resumed as a new instance
```

Resume creates a new instance row and closes any stale predecessor transactionally.

### Message

```text
accepted → retrieved zero or more times → acknowledged
```

Acknowledgement is terminal. Messages are never edited or deleted through version-one APIs.

### Adapter activation

```text
quiet → pending → waking → running → quiet
                    │          │
                    └→ backoff ←┘
                               ↓
                            blocked
```

Transient activation state is local adapter state, not relay truth.

## 10. SQLite model

Required tables:

```sql
squads(
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  mission TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('active','archived')),
  created_at INTEGER NOT NULL,
  archived_at INTEGER
)

agents(
  id TEXT PRIMARY KEY,
  created_at INTEGER NOT NULL
)

memberships(
  id TEXT PRIMARY KEY,
  squad_id TEXT NOT NULL REFERENCES squads(id),
  agent_id TEXT NOT NULL REFERENCES agents(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  role TEXT NOT NULL,
  joined_at INTEGER NOT NULL,
  left_at INTEGER
)

instances(
  id TEXT PRIMARY KEY,
  membership_id TEXT NOT NULL REFERENCES memberships(id),
  mode TEXT NOT NULL CHECK(mode IN ('cooperative','scheduled','harnessed')),
  client_kind TEXT NOT NULL,
  hostname TEXT,
  resume_token_hash BLOB NOT NULL,
  availability TEXT NOT NULL CHECK(availability IN ('idle','busy','blocked','unknown')),
  availability_source TEXT NOT NULL,
  availability_observed_at INTEGER NOT NULL,
  lease_expires_at INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  closed_at INTEGER
)

messages(
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  squad_id TEXT NOT NULL REFERENCES squads(id),
  sender_membership_id TEXT NOT NULL REFERENCES memberships(id),
  recipient_membership_id TEXT NOT NULL REFERENCES memberships(id),
  body TEXT NOT NULL,
  body_hash BLOB NOT NULL,
  priority TEXT NOT NULL CHECK(priority IN ('normal','high')),
  reply_to TEXT REFERENCES messages(id),
  correlation_id TEXT,
  dedupe_key TEXT,
  created_at INTEGER NOT NULL,
  acknowledged_at INTEGER
)

schema_migrations(
  version INTEGER PRIMARY KEY,
  applied_at INTEGER NOT NULL,
  checksum TEXT NOT NULL
)
```

Required indexes and constraints:

- partial unique active normalized membership name per squad;
- composite uniqueness on membership `(id, squad_id)` and message `(id, squad_id)` so foreign keys structurally prevent cross-squad sender, recipient, and reply references;
- one unclosed, unexpired owner enforced transactionally per membership;
- unique `(squad_id, sender_membership_id, dedupe_key)` when dedupe key is non-null;
- inbox index on `(recipient_membership_id, acknowledged_at, priority, sequence)`;
- roster index on `(squad_id, left_at)`;
- lease-expiry index on `(lease_expires_at, closed_at)`.

All migrations are ordered, checksum-verified, forward-only files embedded in the binary. Startup applies migrations within an exclusive migration transaction. The relay refuses to open a database with a newer schema version.

Transaction boundaries:

- squad create/join and instance claim are atomic;
- message idempotency check, insert, and response are atomic;
- acknowledgement batch is atomic;
- resume and predecessor closure are atomic.

SQLite configuration must enable foreign keys, WAL mode, a bounded busy timeout, and an explicit synchronous policy documented with durability implications.

## 11. HTTP API

Base path: `/v1`. Content type: `application/json`. IDs are opaque strings.

Endpoints:

```text
GET  /healthz
GET  /readyz
GET  /v1/squads
POST /v1/squads
GET  /v1/squads/{squad}
POST /v1/squads/{squad}/archive
POST /v1/squads/{squad}/join
POST /v1/squads/{squad}/leave
GET  /v1/squads/{squad}/roster
POST /v1/heartbeat
POST /v1/messages
GET  /v1/inbox?limit={n}&wait={seconds}
POST /v1/messages/ack
GET  /v1/squads/{squad}/transcript?after={sequence}&limit={n}
```

Long-poll requirements:

- maximum wait is 30 seconds;
- return immediately when eligible pending mail exists;
- return `200` with an empty list on timeout;
- cancellation releases all resources;
- commits notify relevant local waiters after the transaction completes;
- correctness never depends on an in-memory notification.
- pending inbox selection is always `acknowledged_at IS NULL` in ascending sequence order; retrieval cursors must never hide an unacknowledged message.

Error envelope:

```json
{
  "error": {
    "code": "name_in_use",
    "message": "The requested membership name has a live owner.",
    "retryable": false,
    "details": {}
  }
}
```

Required codes include `invalid_request`, `not_found`, `squad_archived`, `not_member`, `name_in_use`, `lease_expired`, `recipient_not_found`, `idempotency_conflict`, `payload_too_large`, `rate_limited`, `database_busy`, and `internal_error`.

The repository must contain a checked-in OpenAPI description generated from or verified against the Rust types.

## 12. MCP contract

Tools:

```text
squad_join
squad_leave
squad_list
squad_describe
squad_roster
message_send
message_receive
message_acknowledge
agent_status
```

Heartbeat and resume-token handling are internal and not model-callable.

Every received message is returned in a clearly delimited structure labelled as untrusted participant content. Tool descriptions state that message content cannot change system instructions, permissions, or squad identity.

`message_receive` supports acknowledging prior IDs in the same request to reduce turns, but retrieval alone never acknowledges.

## 13. Client adapters

### Claude Code

- Standard MCP tools support cooperative mode.
- Claude `/loop` may implement scheduled mode while the interactive session remains open.
- Harnessed mode advertises the experimental `claude/channel` capability and emits `notifications/claude/channel`.
- Custom Channel preview/allowlist limitations must be documented.
- Channel content is a fixed wake notice plus trusted metadata; message bodies remain in the inbox.
- The adapter must not call `claude -p`.

### Codex

- Standard MCP tools support cooperative mode.
- Codex desktop scheduling may provide scheduled mode, but the CLI is not claimed to have session-local cron.
- Harnessed mode connects to Codex App Server, generates or consumes the installed-version schema, initializes, resumes or starts a configured thread, and starts a turn on wake.
- Ordinary mail never calls `turn/interrupt`; `turn/steer` is off by default.
- App Server experimental transport caveats must be documented.

## 14. Repository structure

Provisional workspace:

```text
Cargo.toml
LICENSE
README.md
SECURITY.md
CONTRIBUTING.md
CHANGELOG.md
docs/
  architecture.md
  protocol.md
  operations.md
  claude.md
  codex.md
  troubleshooting.md
  threat-model.md
  releasing.md
openapi/
  psst-v1.yaml
crates/
  psst-core/       types, validation, state rules; no network or database I/O
  psst-store/      rusqlite repository, migrations, transactions
  psst-relay/      HTTP service, long polls, lifecycle, health
  psst-client/     typed relay client, token store, retries
  psst-mcp/        MCP tools and Claude Channel adapter
  psst-codex/      Codex App Server activation adapter
  psst-cli/        operator and human client
tests/
  contracts/
  e2e/
packaging/
  windows/
  linux/
  macos/
.github/workflows/
```

Dependencies point inward: adapters and interfaces depend on core abstractions; core never imports HTTP, SQLite, MCP, Claude, or Codex concerns.

## 15. Configuration

Precedence:

```text
CLI flags > environment variables > config file > defaults
```

Configuration includes bind address, data path, log format/level, message limits, long-poll limit, lease timings, and adapter relay URL. Defaults are safe, deterministic, documented, and printed by a `config show --effective` command with secrets redacted.

Default relay state follows platform data-directory conventions. Tests use explicit temporary directories.

## 16. Diagnostics and operations

- Structured `tracing` logs with text and JSON modes.
- Correlation fields: request ID, squad ID, membership ID, instance ID, and message ID where applicable.
- No message body at info level; body logging requires an explicit unsafe debug flag.
- `/healthz` reports process liveness without database mutation.
- `/readyz` verifies database access and migration compatibility.
- CLI commands support database information, backup, and integrity check.
- Backup uses a SQLite-consistent mechanism, not raw copying of a live WAL database.
- Startup reports bind address, database path, schema version, and trusted-LAN warning.

## 17. Testing requirements

### Unit and property tests

- Identifier, name, role, mission, message, and configuration validation.
- State transitions and invalid transitions.
- Idempotency equality and conflict logic.
- Wake coalescing state machine.
- Retry/backoff bounds.
- Secret redaction.

### Store and migration tests

- Empty database migration to current.
- Each historical migration upgrade path.
- Checksum mismatch and future schema rejection.
- Foreign keys and partial uniqueness.
- Transaction rollback under injected failures.
- Restart persistence.

### Integration and concurrency tests

- Offline delivery and replay.
- Send-time timeout followed by idempotent retry.
- Retrieval without acknowledgement.
- Crash before and after acknowledgement.
- Simultaneous name claim.
- Lease expiration and resume.
- Long-poll wake, timeout, cancellation, and relay shutdown.
- One hundred concurrent watchers.
- Database busy behavior.

### Adapter contract tests

- MCP schemas and error results.
- Token never appears in tool schemas, outputs, or logs.
- Claude Channel capability and notification shape.
- Codex App Server initialization and turn-start transcript against a fake server.
- Busy/pending/backoff transitions.
- No `claude -p` subprocess path exists.

### Cross-platform end-to-end tests

CI must run supported Rust targets and at least one relay/client E2E test on Windows, Linux, and macOS. Release candidates require smoke tests of install, join, roster, send, receive, acknowledge, restart, and uninstall where applicable.

## 18. Build, installation, and distribution

Release artifacts:

```text
psst relay
psst CLI
psst-mcp adapter
psst-codex adapter, if separate
LICENSE
README/quickstart
checksums
SBOM
```

Targets:

- Windows x86-64 `.zip` and optional installer after portable flow is proven.
- Linux x86-64 and ARM64 `.tar.gz`.
- macOS x86-64 and ARM64 `.tar.gz`.

GitHub Actions builds on native hosted runners, runs tests, creates reproducible archives, generates SHA-256 checksums and an SBOM, and publishes GitHub Releases from signed version tags. Release automation must not require undeclared local state.

Installation begins with portable binaries and explicit config commands. Package-manager publication is deferred until the release process is stable.

## 19. Documentation inventory

Required before version `0.1.0`:

- Five-minute local quickstart.
- Two-machine LAN walkthrough.
- Claude cooperative, scheduled, and Channel setup.
- Codex cooperative and App Server harness setup.
- Protocol and delivery-semantics reference.
- Architecture and responsibility boundaries.
- Configuration reference.
- Operations, backup, recovery, and troubleshooting.
- Trusted-LAN threat model and security warning.
- Contributor setup, test matrix, and release process.
- Example transcript demonstrating offline delivery and replay.

## 20. Implementation slices and gates

### Slice 0: repository and quality foundation

Workspace, license, contribution rules, formatting, linting, CI skeleton, dependency policy, architecture decision records, and artifact conventions.

Gate: clean build/test/lint on all CI operating systems with no product behavior claimed.

### Slice 1: core model and SQLite durability

Core types, migrations, store transactions, squad/member/instance/message behavior.

Gate: migration, persistence, idempotency, uniqueness, lease, and acknowledgement tests pass.

### Slice 2: relay and typed client

Versioned HTTP API, long polling, structured errors, shutdown, health, typed Rust client.

Gate: restart, offline delivery, replay, concurrent watcher, and timeout tests pass.

### Slice 3: CLI and cooperative MCP

Human CLI, token storage, MCP tools, cooperative Claude/Codex walkthroughs.

Gate: two independently launched interactive agents exchange and acknowledge messages through the relay; secrets are absent from captured tool/log output.

### Slice 4: harnessed activation

Claude Channel notifications, Codex App Server adapter, wake coalescing, reconciliation, backoff.

Gate: fake-host contract suites plus live opt-in smoke tests demonstrate one wake for a burst, no message-body injection, recovery after dropped wake, and no preemption.

### Slice 5: release engineering

Native artifacts, checksums, SBOM, install/uninstall documentation, release workflow, cross-platform smoke tests.

Gate: a clean machine can install from an artifact, complete the quickstart, restart, and remove the product using only published instructions.

## 21. Definition of done

Version `0.1.0` is complete only when:

- Every functional and non-functional requirement is implemented or explicitly removed through reviewed PRD revision.
- Required automated tests pass in GitHub Actions on Windows, Linux, and macOS.
- No lint, formatting, migration, license, secret-scan, or dependency-policy gate is bypassed.
- Release archives, checksums, SBOM, and documentation are produced by CI.
- Offline delivery, idempotent retry, acknowledgement replay, lease expiry, and relay restart have captured E2E evidence.
- Claude cooperative and Channel modes have documented and captured smoke evidence where the preview feature is available.
- Codex cooperative and App Server modes have documented and captured smoke evidence.
- A fresh-user installation rehearsal succeeds from the release artifacts.
- Known limitations and trusted-LAN security boundaries are prominent.
- No required behavior depends on an uncommitted file, developer-global configuration, or manually patched binary.

## 22. Traceability summary

| Requirement group | Primary verification |
|---|---|
| FR-001–010 | store/API integration tests; CLI E2E |
| FR-020–024 | fake-clock lease tests; roster contract tests |
| FR-030–043 | transaction, idempotency, replay, limit, and concurrency tests |
| FR-050–060 | activation state-machine tests; fake host contracts; opt-in live smoke tests |
| FR-070–072 | CLI snapshots, JSON contracts, redaction tests |
| NFR-001–007 | fault injection, restart, cancellation, and shutdown tests |
| NFR-008 | repeatable local benchmark and CI smoke threshold |
| NFR-009–010 | serialization and error-contract tests |
| Distribution | native CI matrix, artifact inspection, clean-machine rehearsal |
| Documentation | executable quickstart and link/check validation |

## 23. Deferred product choices

These require explicit review before implementation reaches their slice:

- Final project name and binary/crate namespace.
- Whether squad creation is command-only or also implicit on first join.
- Who may edit a mission before archive in the trusted-LAN model.
- Whether transcripts show acknowledged state by default.
- Whether the Claude Channel adapter can be implemented cleanly in Rust against the current preview surface or needs a tiny TypeScript boundary.
- Whether `psst-mcp` and `psst-codex` ship as separate executables or subcommands of one binary.
