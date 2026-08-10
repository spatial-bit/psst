# W-403: Claude Code Channel adapter

Status: verified

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

## Candidate evidence

- `psst-mcp` accepts one closed process opt-in (`PSST_CLAUDE_CHANNEL=1|true|enabled`). Without it,
  initialization remains the unchanged cooperative MCP surface. With it, initialization advertises
  exactly `experimental.claude/channel` and the durable relay instance is registered/resumed as
  `harnessed`.
- An unbound Channel server initializes safely. After `squad_join` durably binds the profile, the
  server attaches the client-neutral activation runtime to the already-initialized Claude peer.
  Clean leave stops observation before the terminal profile mutation; a recoverable leave failure
  reconciles from relay truth without resurrecting an ambiguous terminal state.
- Every activation sends `notifications/claude/channel` with fixed instructions plus bounded
  profile, squad, aggregate pending count/priority, and oldest message ID strings. Participant
  bodies, routing fields, credentials, authorization, and resume tokens are absent from capability,
  notification, and fixed diagnostic bytes.
- Notification transport failure blocks instead of retrying an ambiguously issued model turn. A
  successfully written notification remains occupied until the notified oldest pending message is
  acknowledged or the authoritative oldest ID changes. Completion reconciliation is paced to at
  most one request per second; the paused-clock regression proves unchanged pending mail cannot
  spin. Silent preview/policy drops therefore become a bounded visible blocked diagnostic rather
  than a notification or polling flood.
- The first wake is held for a fixed one-second settle interval after Claude's standard MCP
  initialization notification. A paused-clock regression proves the transport cannot emit during
  that client-side Channel registration window; this closes a live startup race where transport
  success preceded Claude's handler registration and the pending wake was silently lost.
- The in-memory fake-Claude transcript proves the exact SDK notification method and parameters.
  A real relay/two-process test proves pending mail emits one body-free wake, retrieval replays
  without acknowledgement, explicit acknowledgement clears it, roster mode is `harnessed`, clean
  leave is silent, and restart reconciliation wakes for mail accepted while the adapter was down.
- Operator documentation makes the development-channel flag and organization policy explicitly
  operator-owned, states that transport write is not model processing, and forbids permission
  relay, `claude -p`, client launch, network listeners, and implicit acknowledgement.
- Local focused gates: all `psst-mcp` targets pass (13 unit tests, 2 real-relay process tests, and 4
  stdio protocol tests); strict package Clippy with all targets/features and `-D warnings` passes;
  formatting passes.
- Opt-in live Claude Code 2.1.226 smoke on Windows passed against the real relay and the preserved
  pending message `msg_803cfff266a42d053fca16654b70c4ed`. Claude registered the Channel handler
  at `2026-08-10T16:38:07.987Z`; Psst emitted the first wake at `16:38:08.802Z`, after the fixed
  registration fence. Without an operator prompt, Claude called `message_receive` at
  `16:38:13.985Z` and `message_acknowledge` at `16:38:16.244Z`; the relay durably committed the
  acknowledgement at `16:38:16.264Z`. Relay evidence shows reconciliation stayed at approximately
  one request per second while pending, then returned to ordinary long polling after acknowledgement.
  The Channel emitted no duplicate wake, and the adapter consumed 0.14 CPU seconds during the
  observed live run. Participant content and credentials are excluded from this evidence record.
- The final repaired head `243c2a93da2240a5754f9a6bf46ad281041cd904` merged through PR
  [#7](https://github.com/spatial-bit/psst/pull/7). It includes the bounded SQLite startup window
  and the contended committed-wake deadline repair. Standard CI passed on Windows, Ubuntu, and
  macOS in [workflow 31414087531](https://github.com/spatial-bit/psst/actions/runs/31414087531),
  and all three native dogfood builds passed in
  [workflow 31414087571](https://github.com/spatial-bit/psst/actions/runs/31414087571).
