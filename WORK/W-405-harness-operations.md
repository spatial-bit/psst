# W-405: Harness configuration and operations

Status: planned

## Objective

Make Claude and Codex harnesses operable with explicit profile/thread ownership, status, diagnostics, startup, and clean shutdown across supported platforms.

## Dependencies

- W-403 and W-404 verified.

## Acceptance

- Operators can configure, start, inspect, and stop one harness per profile without exposing credentials or silently mutating client configuration.
- Existing profile locking prevents cooperative and harness processes from concurrently owning one identity.
- Status distinguishes quiet, pending, waking, running, backoff, blocked, and stopped with bounded non-secret diagnostics.
- Crash/restart preserves relay truth, reconciles pending mail, and cannot duplicate an already-running host turn.
- Windows, Linux, and macOS process/signal/path behavior has native tests.
