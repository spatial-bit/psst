# W-604: Alpha.2 mail-awake fleet gate

Status: verified at exact main revision `a52d0219bcd0aff4cbafc9faab5c9bc9ec7fbc50`

## Objective

Verify the complete multi-team, mail-awake alpha.2 candidate from downloaded native archives.

## Acceptance

- Windows x86-64, Linux x86-64, and macOS ARM64 build and checkoutless package gates pass.
- One real relay hosts at least two isolated squads with overlapping identities and messages.
- Claude Channel and Codex App Server harnesses wake from durable pending mail without duplicate or
  lost work, body injection, active-turn preemption, or cross-squad activation.
- The bundled agent guide completes from a clean environment.
- No tag or publication occurs without a separate exact owner authorization.

## Evidence

- The native harness starts one real relay with `w604-primary` and `w604-decoy`. Both squads use
  overlapping `codex`, `claude`, and `sender` names under distinct profiles and credentials.
- Decoy mail is committed first. The primary Claude Channel emits zero notifications and the
  primary Codex App Server emits zero turns. Each decoy recipient later retrieves exactly its own
  pending message and acknowledges it.
- Primary mail then proves Claude wake, replay-before-ack, explicit acknowledgement, restart
  reconciliation, and Codex idle wake, one turn, acknowledgement, and clean stop.
- Merged through PR [#19](https://github.com/spatial-bit/psst/pull/19). A coherent local Windows
  `0.1.0-alpha.2` release build passed the complete gate twice. The exact main revision passed the
  same native and clean-downloaded/no-Rust gate on Windows x86-64, Linux x86-64, and macOS ARM64 in
  [workflow 31454840050](https://github.com/spatial-bit/psst/actions/runs/31454840050). Sanitized
  retained evidence contains identities and message IDs but no bodies, credentials, or client
  stdout/stderr.
