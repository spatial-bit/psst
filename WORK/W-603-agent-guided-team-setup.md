# W-603: Agent-guided team setup and operations

Status: planned

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
