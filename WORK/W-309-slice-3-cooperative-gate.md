# W-309: Slice 3 reliability and cooperative dogfood gate

Status: blocked on W-301 through W-308

## Objective

Independently validate the complete cooperative CLI/MCP slice through automated two-process tests and a live Claude Code/Codex walkthrough.

## Dependencies

- W-301 through W-308 implemented and separately reviewed.

## Acceptance

- Automated native tests launch two isolated profile roots and two `psst-mcp` processes, join distinct identities, prove heartbeat presence, bidirectional send, replay before acknowledgement, acknowledgement, reply, adapter restart/resume, relay restart, and lease-derived offline presence.
- Captured JSON-RPC, CLI output, stderr, relay logs, and evidence are scanned for credential/authorization canaries and unintended message-body logging.
- Windows, Linux, and macOS run the real relay/CLI/MCP gate and clean downloaded artifacts.
- One interactive Claude Code session and one interactive Codex session, each configured with `psst-mcp` and distinct profiles, voluntarily exchange, replay, acknowledge, reply, inspect roster, and demonstrate heartbeat without prompt mode, wake, Channels, App Server, or keystroke injection.
- Exact client/artifact revisions and a sanitized reproducible transcript are recorded; an independent non-implementer approves the gate.

## Verification evidence

Pending.
