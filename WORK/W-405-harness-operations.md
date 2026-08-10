# W-405: Harness configuration and operations

Status: verified

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

## Candidate evidence

- The application owns one versioned, profile-keyed, non-secret status record shared by the Claude
  Channel and Codex App Server adapters.
- `psst --profile <profile> harness status` reads that record passively without competing for the
  profile lock and classifies it as recent, stale, or stopped without claiming process liveness.
- Both adapters publish bounded phase, retry, pending-count, priority, owner-PID, and timestamp
  state; message bodies, message IDs, credentials, authorization values, and tokens are excluded.
- Deterministic tests cover the startup publication boundary, running-to-stopped shutdown, and an
  abruptly aborted publisher whose abandoned record is overwritten by a restarted owner before a
  clean terminal publication. A Unix test rejects a symlink status target.
- Foreground start/stop and crash reconciliation are documented in
  `docs/harness-operations.md`; the relay inbox and profile lock remain authoritative.
- Focused application, CLI, MCP, and Codex tests and strict cross-crate Clippy are green locally.

Exact head `5b5fd7128e42bedf14a83fa011b045e30aebd646` merged through PR
[#9](https://github.com/spatial-bit/psst/pull/9). Standard CI, including native process/status
tests, passed on Windows, Ubuntu, and macOS in
[workflow 31414147304](https://github.com/spatial-bit/psst/actions/runs/31414147304); all three native
dogfood builds passed in
[workflow 31414147342](https://github.com/spatial-bit/psst/actions/runs/31414147342).
