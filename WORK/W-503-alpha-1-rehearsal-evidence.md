# W-503: v0.1.0-alpha.1 rehearsal and evidence bundle

Status: planned

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
- A separate reviewer-attestation workflow binds an independent approval to the exact tag, revision,
  candidate workflow run, and `SHA256SUMS` bytes. Candidate evidence remains pending until then.

## Evidence

Pending W-502 and candidate CI.
