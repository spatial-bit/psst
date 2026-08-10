# W-404: Codex App Server adapter

Status: local implementation and real Windows wake candidate; native CI pending

## Objective

Connect the shared activation engine to one durable Codex thread through the installed Codex App Server protocol.

## Dependencies

- W-402 verified.

## Allowed scope

- New `psst-codex` crate/binary, local stdio App Server process ownership, installed-version schema generation/validation, handshake, thread policy, turn lifecycle, bounded queues, and fake/real host tests.

Do not use remote unauthenticated WebSockets, `turn/steer`, `turn/interrupt`, or participant message bodies as turn input.

## Acceptance

- The adapter performs exactly one `initialize` then `initialized`, resumes the configured durable thread or creates one only under explicit policy, and calls `turn/start` with fixed wake instructions.
- It consumes the installed-version schema or fails closed on incompatible methods/fields; protocol stdout remains pure and bounded.
- `turn/completed`, overload, process exit, timeout, malformed frames, cancellation, and restart map exactly into the shared host outcome model.
- An active turn is never interrupted or steered for ordinary mail; mail pending at completion is reconciled.
- Fake App Server transcripts prove all boundaries; an opt-in real installed Codex smoke proves idle wake and durable thread resume.

## Candidate evidence

- The exact installed Codex `0.147.0` schema generator passes the closed initialize, thread
  start/resume, turn start, and completion shape checks on Windows.
- Focused adapter tests cover the exact handshake, body-free fixed wake input, explicit rejection,
  timeout, malformed/oversized/closed frames, completion identity/status, and forbidden
  steer/interrupt absence.
- The adapter injects one process-scoped `psst-mcp` definition instead of mutating or depending on
  Codex's global MCP registry. Inbox observation releases the profile before App Server launch and
  resumes only after App Server and MCP are reaped.
- Codex App Server does not automatically place configured MCP tools in a programmatic thread's
  model context. The adapter now uses the documented experimental `dynamicTools` contract and
  proxies exactly receive/acknowledge callbacks through `mcpServer/tool/call`. The receive schema
  forbids implicit acknowledgement, and the adapter accepts acknowledgements only for IDs returned
  by receive in that same turn.
- Installed Codex `0.147.0` exposed and closed two live protocol gaps: `thread/start.sandbox` uses
  the wire enum `workspace-write`, and notifications received before their request response are
  retained for the completion loop instead of being discarded.
- A real Windows loopback run with installed Codex `0.147.0` woke a fresh durable thread for
  `msg_f4895618479c4bb7fb931b98d9040716`, called the two dynamic tools, acknowledged only after
  retrieval, and left `pending_count: 0`. A second message
  `msg_a864f06018690ada47c9632d4826ff66` repeated the result after the retrieve-before-ack fence was
  added. Both wake prompts were body-free and used no shell or filesystem fallback.
- Strict workspace Clippy, formatting, patch integrity, Slice 4 contract tests, and the complete
  locked workspace test suite pass locally.
- Native Windows/Ubuntu/macOS CI and an automated durable-resume smoke remain required before this
  work unit is verified.
