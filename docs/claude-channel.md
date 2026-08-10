# Claude Code Channel harness

Status: Slice 4 implementation candidate. The Channel interface is an experimental Claude Code
preview and is not part of the `v0.1.0-alpha.1` release surface.

For the shared foreground start, status, stop, and restart workflow, see
[Wake harness operations](harness-operations.md).

Psst can extend its existing stdio MCP server with Claude Code's one-way Channel capability. In
this mode the adapter observes the durable inbox and sends a fixed wake notification when mail is
pending. Claude still retrieves mail with `message_receive` and must explicitly acknowledge
completed messages with `message_acknowledge`.

## Safety boundary

- Channel mode is off unless the MCP child has `PSST_CLAUDE_CHANNEL=enabled`. The only other
  accepted opt-in values are `1` and `true`; every other value fails startup closed.
- The initialization response advertises `experimental.claude/channel` only after that opt-in.
- Psst sends `notifications/claude/channel` with fixed instructions and bounded metadata:
  profile, squad, pending count, aggregate highest priority, and oldest pending message ID.
- Participant message bodies, sender-controlled routing fields, credentials, authorization values,
  and resume tokens never enter the wake notification or Channel diagnostics.
- A successful notification write does not prove Claude processed the wake. Psst keeps the turn
  occupied until relay truth shows that the notified oldest message was acknowledged. A silent
  drop, disconnect, or unconsumed wake eventually blocks activation instead of issuing a duplicate
  model turn.
- Psst does not relay Claude permission prompts, start Claude, use `claude -p`, inject keystrokes,
  add a network listener, or change Claude configuration.

## Operator-owned setup

An unbound Channel-enabled server initializes normally. Its `squad_join` creates a harnessed relay
instance and starts inbox observation only after the profile binding is durable. A previously bound
profile resumes in harnessed mode when this process becomes its owner.

Configure the `psst-mcp` stdio child with the same `PSST_RELAY` and `PSST_PROFILE` values as that
bound profile, plus:

```text
PSST_CLAUDE_CHANNEL=enabled
```

Custom Channels currently require Claude Code's development-channel preview flag. The operator,
not Psst, supplies that flag when starting the interactive Claude session and explicitly names the
registered MCP server. Confirm the exact syntax supported by the installed Claude version against
the current [Claude Code Channels reference](https://code.claude.com/docs/en/channels-reference).
Psst deliberately does not set or infer the preview flag.

Organization policy can disable Channels, and Claude can silently drop Channel events when the MCP
server was not loaded as a Channel. Psst cannot distinguish those cases from an unprocessed wake at
the transport boundary. If the fixed stderr diagnostic reports that Channel activation is blocked,
verify the installed preview support, organization policy, named Channel load, profile ownership,
and stdio connection before restarting that profile. Restart reconciliation reads the durable
inbox; it never treats a wake as delivery or acknowledgement.

## Expected wire contract

Harnessed initialization adds only this experimental capability:

```json
{
  "experimental": {
    "claude/channel": {}
  }
}
```

Each wake is a JSON-RPC notification shaped like:

```json
{
  "jsonrpc": "2.0",
  "method": "notifications/claude/channel",
  "params": {
    "content": "Psst has durable pending mail. Use message_receive to inspect it; retrieval does not acknowledge. Process the pending work, then explicitly call message_acknowledge for each completed message.",
    "meta": {
      "profile": "bound-profile",
      "squad": "bound-squad",
      "pending_count": "2",
      "highest_priority": "high",
      "oldest_message_id": "msg_example"
    }
  }
}
```

Metadata values are strings because that is the documented Claude Channel contract. The inbox,
not the notification, remains authoritative.
