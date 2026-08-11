# W-603: Agent-guided team setup and operations

Status: verified at exact main revision `a52d0219bcd0aff4cbafc9faab5c9bc9ec7fbc50`

## Objective

Ship a bundled, agent-readable, soup-to-nuts guide that lets a Claude or Codex agent safely walk a
user through installing Psst, creating one or more teams, assigning profiles, enabling wake harnesses,
verifying mail behavior, operating the hub, troubleshooting, and cleaning up.

## Acceptance

- Commands are directly executable on PowerShell and POSIX from downloaded archives.
- The guide branches cleanly for Codex, Claude, cooperative-only, loopback, and trusted-LAN use.
- Multi-team setup uses one relay and one profile/process per membership and explains the admission
  boundary without claiming hostile tenancy.
- The journey proves replay-before-ack, explicit acknowledgement, restart/resume, wake-on-mail, and
  safe leave/stop behavior.
- A checkoutless agent-followed rehearsal validates the document against the packaged binaries.

## Evidence

- `docs/team-setup-agent-guide.md` is a single ordered runbook for artifact verification, topology
  planning, one-hub/multi-squad setup, unique membership profiles, cooperative MCP, Claude Channel,
  Codex App Server, replay/ack/reply, restart/resume, wake isolation, diagnostics, leave, and stop.
- The guide gives directly executable PowerShell and POSIX archive commands and instructs the
  assisting agent to ask before topology or configuration changes, fail closed on identity
  mismatches, and never inspect or expose credentials.
- Merged through PR [#18](https://github.com/spatial-bit/psst/pull/18). The alpha.2 packager and
  inspector include the guide as `TEAM-SETUP.md` in their exact manifest-controlled inventory. A
  closed guide verifier requires the ordered topology, security, PowerShell/POSIX, multi-team,
  wake, replay/ack, restart, troubleshooting, and cleanup contract and has a negative regression.
  Every native and clean-downloaded job validates that bundled guide before running the real fleet
  journey in [workflow 31454840050](https://github.com/spatial-bit/psst/actions/runs/31454840050).
