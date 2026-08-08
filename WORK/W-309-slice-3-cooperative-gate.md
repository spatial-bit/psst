# W-309: Slice 3 reliability and cooperative dogfood gate

Status: automated native and clean-download gates verified at `a4af73a`; live Claude Code/Codex sessions pending

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
  for all generated authorizations, an environment canary, and both message bodies.
- The live-proof retention contract binds its sanitizer declaration to the same fixed harness canary,
  `w309-authorization-canary-must-not-ship`, by SHA-256 and rejects the literal canary or credential
  keys in the retained sanitized JSON.
- The native Windows release binaries passed the complete harness repeatedly (39.8, 38.9, 39.0,
  40.8, and 40.8 seconds across implementation and repair boundaries). Independent adversarial
  review approved the final process cleanup, revision binding, credential confinement, and evidence
  scans.
- The workflow runs the harness on Windows x86-64, Linux x86-64, and macOS ARM64 build outputs, then
  downloads the same harness separately and repeats it against the clean downloaded archive without
  a checkout or Rust toolchain. At revision `a4af73ad800dde8ceff8209768685e0d7cf19809`, all three
  native jobs and all three checkoutless jobs passed in
  [workflow 31274817025](https://github.com/spatial-bit/psst/actions/runs/31274817025). Standard
  Windows, Ubuntu, and macOS CI passed in
  [workflow 31274551562](https://github.com/spatial-bit/psst/actions/runs/31274551562).
- Interactive Claude Code and Codex walkthroughs are deliberately not claimed and remain pending.
