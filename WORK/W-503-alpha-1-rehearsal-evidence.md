# W-503: v0.1.0-alpha.1 rehearsal and evidence bundle

Status: checkoutless rehearsal contract hardened; exact-tag execution and external evidence pending

## Objective

Rehearse portable install, local quickstart, restart, message replay and acknowledgement, MCP
initialization, and uninstall from downloaded assets without Rust or a checkout. Validate the LAN
configuration contract in automation, but reserve an actual non-loopback start for an
owner-controlled isolated trusted network so CI never exposes an unauthenticated relay.

## Acceptance

- Each claimed native target passes archive inspection and clean-download install/uninstall smoke.
- The retained evidence bundle records source revision/tag, CI URLs, platform matrix, migration and
  fault evidence references, artifact manifest/hashes, SBOM, rehearsal report, limitations, and a
  signed-off requirement traceability table without secrets or user data.
- Documentation alone is not accepted as rehearsal evidence.
- Before W-504, an owner-controlled isolated-LAN rehearsal must retain its bind/origin/firewall
  configuration and health result. Hosted candidate CI does not claim non-loopback LAN operation.
- That rehearsal must inject the fixed non-secret canary
  `w503-lan-authorization-canary-must-not-retain` into its credential-leak scan. Its sanitized JSON
  proof records the SHA-256 of that documented canary, never the canary bytes or credentials.
- A separate reviewer-attestation workflow binds an independent approval to the exact tag, revision,
  candidate workflow run, and `SHA256SUMS` bytes. Candidate evidence remains pending until then.

## Evidence

- The exact-tag candidate workflow defines checkoutless, no-Rust archive inspection, checksum and
  manifest verification, portable install, local relay health, two-profile roster/send/replay,
  relay restart, explicit acknowledgement, MCP initialization/stdout purity, uninstall, and user
  data retention on all three supported native runners.
- Downloaded checksum input is accepted only as one exact lowercase SHA-256 line bound to the
  canonical target archive name. The messaging rehearsal proves the same unacknowledged message
  replays after relay restart and that explicit acknowledgement leaves no message or pending count.
- The workflow also defines a sanitized evidence bundle that binds the tag, revision, workflow run,
  target matrix, hashes, SBOM location, rehearsal, limitations, and explicit external prerequisites.
- Execution against a signed tag, an owner-controlled isolated trusted-LAN rehearsal, W-309 live
  Claude/Codex evidence, independent reviewer attestation, and publication approval remain pending.
