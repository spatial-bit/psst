# W-305: Human squad and messaging CLI

Status: verified for the accepted CLI contract; optional process-boundary ambiguity test remains authorization-gated

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

- Windows local candidate: `psst-cli` has 14 passing unit tests and three passing real
  child-process tests. The real two-profile journey covers cooperative join and protected
  credential persistence, stdin only through explicit `--file -`, replay before acknowledgement,
  explicit acknowledgement, transcript paging, offline delivery, relay restart and adapter-owned
  resume, listen heartbeat/Ctrl-C cancellation, leave/archive, input bounds, and secret
  confinement. The initial three-test process suite completed in 51.24 seconds. After strengthening
  the scan to cover both live credentials, the two-profile journey passed again in 49.21 seconds;
  its lock-acquisition probe now sleeps between bounded attempts and detects premature listener exit
  instead of spawning an unbounded tight loop. A subsequent 55.73-second pass also proves a
  wrong-squad archive returns `authority_denied`, awaits runtime shutdown, and permits immediate
  reuse of the same profile lock.
- A focused failed-leave seam proves a recoverable leave error still awaits runtime shutdown. The
  command preserves the more specific mapped leave failure if shutdown also fails, so cleanup does
  not replace the user-visible operation outcome with a secondary reaping error.
- Every protected operation routes through `SessionRuntime`; raw `Client` use is limited to
  constructing the runtime and preparing one logical send. This preserves the runtime's authority,
  cancellation, operation-gate, read-epoch, and deduplicated-send-ledger boundaries.
- `cargo clippy -p psst-cli --all-targets --locked -- -D warnings`,
  `cargo fmt -p psst-cli -- --check`, and `git diff --check` pass in the stabilized workspace.
- Independent adversarial review approved the runtime routing, input/output contracts, process
  lifecycle, and repaired shutdown paths. Failed leave and wrong-squad archive both await bounded
  runtime shutdown; immediate profile reuse and error-precedence regressions pass.
- The ambiguous-commit CLI proxy test remains deliberately unexecuted because it would forward
  ephemeral test credentials. It requires explicit user authorization. Exact retry is currently
  established below the CLI boundary by the approved client/runtime evidence, not by that pending
  process-level proxy test.
- The integrated candidate revision `a4af73ad800dde8ceff8209768685e0d7cf19809` passed the complete
  workspace test, strict Clippy, and format gate on Windows, Ubuntu, and macOS in
  [workflow 31274551562](https://github.com/spatial-bit/psst/actions/runs/31274551562). The separate
  loopback proxy test is not claimed: it remains behind explicit authorization because it forwards
  ephemeral test credentials. Approved client/runtime evidence establishes exact prepared-send
  retry below the CLI process boundary.
