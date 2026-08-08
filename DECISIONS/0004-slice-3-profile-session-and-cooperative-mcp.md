# ADR 0004: Slice 3 profile, session, and cooperative MCP boundaries

Status: accepted

## Context

Slice 3 adds durable local identity, a human CLI, and cooperative MCP tools for already-running Claude Code and Codex sessions. The relay credential is a continuity secret: exposing it through ordinary configuration, command arguments, environment variables, MCP schemas, model-visible results, or diagnostics would violate the security boundary. Two local processes concurrently using one profile could also heartbeat or resume the same identity and fence one another.

## Decision

- A shared application/session layer owns validated configuration, named profiles, the lifetime profile lock, credential persistence and rotation, join/resume, heartbeat, cancellation, and sanitized state. CLI and MCP remain thin interfaces over it.
- A profile is scoped by canonical relay origin plus local profile name and represents one durable squad membership. One long-lived adapter process may exclusively own one profile at a time.
- Non-secret configuration and secret credentials use separate versioned files. Credentials are restricted local plaintext secrets for the alpha, protected with platform-appropriate user-only permissions and atomic same-directory replacement. OS keyrings and encryption-at-rest are deferred behind the same boundary.
- Credential import/export is store-owned. `Credential` remains non-serializable and non-displayable; no general public raw-secret accessor is introduced.
- Configuration precedence is field-specific CLI flags, then environment, then config file, then safe defaults. No credential or resume-token environment variable exists. The default relay origin is `http://127.0.0.1:7341`; automatic LAN discovery is deferred.
- Join and resume persist the issued or replacement credential before publishing success. Ambiguous join/resume outcomes are never automatically retried or converted into a new identity.
- The shared runtime sends an immediate heartbeat, then non-overlapping heartbeats on relay-advertised cadence. It never infers `idle` from silence. Explicit `lease_expired` may trigger serialized resume and atomic credential rotation; other failures enter a sanitized degraded state.
- `psst` is the human/operator CLI. `psst-mcp` is a separate stdio MCP server so Claude Code and Codex use the same cooperative surface. The transitional `psst-relay` binary may coexist until release packaging is finalized.
- The cooperative MCP server implements only the MCP subset needed for initialize, ping, tool discovery/invocation, cancellation, and clean stdio shutdown. Stdout is protocol-only; diagnostics use stderr.
- One MCP process uses one startup-selected profile. Credentials, profile-secret paths, mode, sender identity, heartbeat, resume, and dedupe keys are not model-callable arguments.
- `squad_join` is the sole model-callable bootstrap identity choice and is accepted only for an unbound profile. Once bound, protected tools derive squad, sender, and mailbox from the profile session and expose no override for them. A configured relay-origin override that disagrees with a bound profile fails closed after canonical origin comparison.
- Every participant-controlled value is identified as untrusted data. Message results use structured content with a fixed security notice and an `untrusted_body` field; compatibility text is canonical JSON so participant text cannot forge delimiters.
- Retrieval alone never acknowledges. `message_receive` may explicitly acknowledge prior IDs before reading; `message_acknowledge` remains separately available.
- Cooperative mode never watches to activate a model, invokes Claude or Codex, uses `claude -p`, injects keystrokes, emits Channels notifications, or calls Codex App Server. Scheduling and harness activation remain Slice 4.

## Consequences

Slice 3 begins with explicit CLI and MCP contracts, then implements the credential/profile boundary and shared session runtime before wiring commands or tools. Cross-platform credential permissions and lifetime locking require dedicated security evidence. The Slice 3 gate includes automated independent MCP processes and a live voluntary Claude/Codex walkthrough; fake hosts do not replace that live cooperative evidence.
