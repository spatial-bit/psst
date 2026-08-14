# ADR 0006: Unified operator and agent surface

Status: accepted

## Context

The alpha.2 package exposes `psst`, `psst-mcp`, `psst-codex`, and a transitional `psst-relay`.
Those process boundaries kept early contracts testable, but they force an operator or setup agent to
assemble executable paths, environment variables, profile ownership, client-specific preview
flags, and durable task identity. Relay discovery alone would remove an address while leaving most
of that accidental complexity visible.

## Decision

- `psst` becomes the only normal user-facing executable. It owns operator commands, relay startup,
  team setup, and `psst agent <driver>` lifecycle commands.
- `psst internal mcp` is a hidden protocol-only mode used by generated process-scoped MCP
  configuration. It preserves the existing bounded framing, stdout purity, credential, and profile
  ownership contracts.
- `psst agent claude` generates a process-scoped Channel configuration and launches interactive
  Claude Code. It never uses `claude -p`; permission skipping remains an explicit caller flag.
- `psst agent codex` owns one durable task record and runs the existing Codex App Server adapter.
  The default creates a task only when none exists; `--continue` requires an existing record.
- Client-specific code remains in the existing adapter crates. The unified CLI dispatches into
  those libraries; no Claude, Codex, discovery, or process behavior enters `psst-core` or the relay.
- The compatibility `psst-mcp`, `psst-codex`, and `psst-relay` binaries remain temporarily and call
  the same library implementations. Packaging removes them only after native migration evidence.
- Discovery is an explicit setup operation that health-checks candidates and saves one canonical
  relay origin. Runtime commands do not silently select a different relay on every startup.
- Explicit CLI, environment, and config origins retain precedence. Multiple discovery candidates
  fail closed and require a human-visible selection.
- Tailscale discovery may enumerate the local client's known peers and probe a fixed, bounded Psst
  identity endpoint. Same-link multicast discovery is a separate driver. Neither mechanism is
  server authentication; Psst's trusted-network warning and admission boundary remain unchanged.

## Consequences

The milestone proceeds in vertical slices: unify the executable and existing wake adapters; add
saved discovery and invitation setup; then prove one-command native Claude and Codex launches from
downloaded packages. Internal binaries remain independently testable while users converge on one
surface.
