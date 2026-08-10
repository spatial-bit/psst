# W-403: Claude Code Channel adapter

Status: planned

## Objective

Connect the shared activation engine to a running Claude Code session through the documented MCP Channel extension.

## Dependencies

- W-402 verified.

## Allowed scope

- `psst-mcp` capability negotiation, one-way Channel notifications, channel-specific configuration and diagnostics, fake-Claude transcripts, and opt-in live smoke fixtures.

Do not launch `claude -p`, relay permissions, send participant message bodies, or add a network listener.

## Acceptance

- The server advertises `experimental.claude/channel` only in configured harness mode and remains a valid cooperative MCP server otherwise.
- Each activation emits `notifications/claude/channel` with fixed content and bounded trusted metadata only.
- Missing opt-in, unsupported Claude version, organization policy, disconnect, stdout failure, and shutdown fail visibly and safely.
- Preview/development flags are operator-owned and documented; Psst never silently changes Claude configuration.
- Fake-host tests prove capability and notification bytes; an opt-in real session proves pending mail wakes Claude without implicit acknowledgement.
