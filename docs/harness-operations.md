# Wake harness operations

> **Low-level compatibility reference:** The unified `psst agent` surface is described in
> [Start here](start-here.md). Use this page to diagnose the older standalone harness processes.

Status: Slice 4 implementation candidate. These harnesses are local, opt-in development surfaces,
not part of the `v0.1.0-alpha.1` release.

Both wake adapters use the same operator-visible lifecycle:

1. Configure one already-bound Psst profile and the client-specific adapter settings.
2. Start the harness in the foreground.
3. Inspect its last published state with `psst --profile <profile> harness status`.
4. Stop it with Ctrl+C or by closing the owning interactive client.

Before step 1, prove the profile through ordinary cooperative MCP. Then stop that MCP owner cleanly.
The wake harness replaces it as owner; never run two adapter processes with the same profile.

The status command is passive. It does not acquire the profile lock, contact the relay, wake a
client, or mutate configuration. Its JSON envelope reports the adapter, activation phase, retry
attempt, aggregate pending count and priority, owner process ID, observation timestamp, and a
freshness classification. It never stores message bodies, message IDs, credentials, authorization
values, or tokens.

Phases are `quiet`, `pending`, `waking`, `running`, `backoff`, `blocked`, and `stopped`. A recent
record means only that the harness recently published it; it is not proof that the recorded process
is still alive. A stale record is diagnostic history. The profile lock remains the authority for
exclusive ownership.

## Claude Code Channel

Give the `psst-mcp` child an already-bound `PSST_PROFILE`, its `PSST_RELAY`, and
`PSST_CLAUDE_CHANNEL=enabled`. Start Claude interactively with an explicit, operator-owned Channel
preview opt-in and a process-scoped MCP configuration. For example:

```powershell
claude --strict-mcp-config --mcp-config C:\path\to\psst-mcp.json `
  --dangerously-load-development-channels server:psst-channel
```

The exact development flag is owned by Claude Code and may change; confirm it against the current
[Claude Code Channels reference](https://code.claude.com/docs/en/channels-reference). Psst does not
edit Claude configuration, start Claude, use `claude -p`, or inject input. Closing the interactive
session stops the child and releases profile ownership.

The startup banner must confirm Channel delivery from the exact named server. A wake carries only
bounded metadata. Claude calls `message_receive` to fetch authoritative mail and calls
`message_acknowledge` only after completing it. Permission skipping is a separate, explicit
operator choice; it is not required by Psst.

See [Claude Code Channel harness](claude-channel.md) for the wake and acknowledgement contract.

## Codex App Server

Configure `PSST_RELAY`, `PSST_PROFILE`, `PSST_CODEX_APP_SERVER=1`, the absolute Codex and
`psst-mcp` command paths, and either an existing durable thread ID or the explicit one-time thread
creation policy documented in [Codex App Server wake adapter](codex-app-server.md). Then run:

```powershell
psst-codex
```

It stays in the foreground and stops on Ctrl+C. The adapter creates a process-scoped MCP child for
each turn and does not alter Codex's global MCP registrations.

Use an existing durable Codex task ID unless the operator deliberately selects the documented
one-time creation policy. On wake, that task reads the authoritative inbox, performs the work, and
acknowledges only completed messages.

## Restart and recovery

After a crash, a prior status record can remain stale. A new owner overwrites it as activation
starts. Relay inbox state—not the status file or a wake notification—is the durable source of truth.
On restart the adapter reconciles that inbox before issuing work. Retrieval still does not
acknowledge; completed messages require explicit acknowledgement.

If status is `blocked`, inspect the adapter's fixed stderr diagnostic, stop any competing owner,
correct the client/profile configuration, and restart the foreground harness. Do not delete profile
credentials or status files as a recovery mechanism.
