# W-305: Human squad and messaging CLI

Status: blocked on W-302 and W-304

## Objective

Complete human identity, roster, messaging, inbox, acknowledgement, transcript, status, and cooperative listen commands over the shared session boundary.

## Dependencies

- W-302 and W-304.

## Acceptance

- Commands cover join, leave, archive, roster, send, inbox, listen, acknowledge, transcript, and status with documented bounds.
- Join forces cooperative mode, persists credentials before success, and never prints them; resume is adapter-owned and accepts no token argument.
- Send reads stdin only through explicit `--file -`, prepares one logical send per invocation, and preserves its dedupe key across internal retry.
- Inbox never implicitly acknowledges; explicit acknowledge-before-read is supported; listen long-polls and heartbeats without wake or auto-ack.
- Ctrl-C cancellation is bounded and process exit does not leave the durable membership.
- Human/JSON snapshots, replay-before-ack, offline delivery, exact retry, restart/resume, bounds, and secret scans pass cross-platform.

## Verification evidence

Pending.
