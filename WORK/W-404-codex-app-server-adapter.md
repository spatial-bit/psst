# W-404: Codex App Server adapter

Status: local implementation candidate; native CI and opt-in real wake evidence pending

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
- Strict workspace Clippy, formatting, patch integrity, Slice 4 contract tests, and the complete
  locked workspace test suite pass locally.
- Native Windows/Ubuntu/macOS CI and an opt-in real idle-wake/durable-resume smoke remain required
  before this work unit is verified.
