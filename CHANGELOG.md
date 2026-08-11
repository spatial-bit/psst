# Changelog

All notable changes will be documented here.

## Unreleased

### Added

- Durable SQLite squads, leases, direct messages, replay, explicit acknowledgement, and transcript
  history behind a versioned relay API and typed client.
- A cross-platform `psst` human CLI with protected profiles and a cooperative `psst-mcp` stdio
  adapter exposing the nine reviewed squad and messaging tools.
- Verified short-retention dogfood archives for Windows x86-64, Linux x86-64, and macOS ARM64.
- Alpha-candidate packaging for the same three targets with normalized portable archives, payload
  manifests, SPDX SBOMs, external SHA-256 checksums, and checkoutless rehearsal automation.
- A `0.1.0-alpha.2` dogfood package candidate containing both mail-awake harnesses, an exact internal
  payload manifest, SPDX SBOM, and an outer archive checksum.
- Explicit same-relay multi-squad isolation for authority, roster, direct mail, acknowledgement,
  transcript, leave, and archive operations, including overlapping member names and operation IDs.
- A bundled agent-readable runbook for setting up and operating one or many cooperative squads with
  Codex, Claude, and the mail-awake harnesses.
- A native and checkoutless fleet gate that sends decoy mail across overlapping squad identities
  and rejects cross-squad Claude notifications or Codex turns.

The `v0.1.0-alpha.1` release is not published. Signed-tag builds, the live Claude/Codex walkthrough,
isolated trusted-LAN rehearsal, independent release attestation, and explicit publication approval
remain required.
