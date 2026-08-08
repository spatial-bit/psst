# W-309: Slice 3 reliability and cooperative dogfood gate

Status: automated native candidate implemented locally; native CI, clean-download CI, independent review, and live sessions pending

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

- A checkout-independent Python harness launches the real relay, CLI, and two isolated `psst-mcp`
  children. It proves distinct joins, online heartbeat presence, bidirectional messages, replay before
  acknowledgement, acknowledgement, reply, adapter restart/resume, same-database relay restart, and
  lease-derived offline presence.
- The harness captures sanitized JSON-RPC plus CLI create/join/leave output, MCP stderr, relay logs,
  the exact required 40-hex source revision, and binary version. It proves each generated
  authorization exists only in its credential record before cleanup and scans observable evidence
  for both authorizations, an environment canary, and both message bodies.
- The native Windows release binaries passed the complete harness twice consecutively (39.8 and
  38.9 seconds). Python syntax and workflow/diff gates remain part of final integrated review.
- The workflow runs the harness on Windows x86-64, Linux x86-64, and macOS ARM64 build outputs, then
  downloads the same harness separately and repeats it against the clean downloaded archive without
  a checkout or Rust toolchain. Those CI jobs have not run yet.
- Interactive Claude Code and Codex walkthroughs are deliberately not claimed and remain pending.
