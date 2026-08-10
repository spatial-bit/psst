# W-309: Slice 3 reliability and cooperative dogfood gate

Status: automated native and clean-download gates verified at `a4af73a`; operator-directed live
Claude/Codex rehearsal passed at `ae55f7b`; post-repair Codex lifecycle smoke passed at `30c77d3`

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
- The operator-directed interactive walkthrough and the post-repair Codex-only lifecycle check are
  recorded below. They are dogfood evidence, not a claim that Psst can launch or wake either client.

## Operator-directed live rehearsal at `ae55f7b`

- A real loopback relay plus interactive Claude Code and Codex CLI sessions joined as distinct
  profiles, reported online heartbeat presence, exchanged messages in both directions, replayed
  each message unchanged before acknowledgement, acknowledged explicitly, preserved the reply and
  correlation links, and drained both inboxes to zero. Claude then restarted and resumed its bound
  profile without another join.
- The rehearsal exposed a Codex-specific lifecycle defect: Codex shares global MCP registrations
  across desktop and CLI tasks, so several idle tasks eagerly started the same bound profile and
  caused handshake failures. The repair defers profile ownership until the first protected tool
  call. A real two-process regression proves concurrent MCP initialization, a typed
  `profile_locked` result only on contended use, and successful handoff after the first owner exits.
- The lifecycle repair passed complete Windows, Ubuntu, and macOS CI plus native cooperative artifact
  gates at `30c77d34c510e851f69d1418ff3eacc09b831cd2` in
  [workflow 31340774319](https://github.com/spatial-bit/psst/actions/runs/31340774319) and
  [workflow 31340774316](https://github.com/spatial-bit/psst/actions/runs/31340774316).

## Post-repair Codex-only lifecycle smoke at `30c77d3`

- Release binaries were built locally from the exact passing PR revision. An isolated loopback relay,
  unique squad/profile, and temporary global Codex MCP registration were used; Claude was not involved.
- An ephemeral Codex CLI `0.147.0` session called `squad_join` and `agent_status` through the real Psst
  MCP server, then exited without leaving. Both protected calls succeeded for profile
  `smoke-30c77d3`.
- A completely fresh ephemeral Codex session used the same registration and profile without calling
  `squad_join`. `agent_status` resumed the binding, `squad_roster` contained `codex-smoke`, and
  `squad_leave` returned `left: true`.
- The temporary MCP registration was removed and the isolated relay was stopped after the pass. This
  closes the fresh-artifact/post-repair operator check created by the `ae55f7b` rehearsal defect.
