# W-501: v0.1.0-alpha.1 release contract

Status: pre-tag contract verified; signed tag and candidate CI remain owner-gated

## Objective

Freeze a truthful, mechanically checked contract for the first portable cooperative alpha without
claiming deferred Slice 4 activation or unsupported platforms.

## Acceptance

- The source tag is exactly `v0.1.0-alpha.1`, is annotated and signed, and points to the tested
  revision; the workspace package version is exactly `0.1.0-alpha.1`.
- Release preparation fails closed when tag, version, revision, dirty state, inventory, or target is
  inconsistent.
- Supported alpha assets are Windows x86-64 ZIP, Linux x86-64 tar.gz, and macOS ARM64 tar.gz only.
- Scope is cooperative CLI and stdio MCP on one machine or a trusted LAN. Scheduling, Claude
  Channels, Codex App Server activation, keystroke injection, installers, package managers, hostile
  networks, and production support are excluded.
- No tag, GitHub Release, or external publication occurs without explicit owner authorization.
- W-309 live Claude-to-Codex cooperative evidence and final Windows/Linux/macOS CI are external
  prerequisites. Asset preparation must not imply those gates passed.

## Evidence

- Workspace packages are pinned to `0.1.0-alpha.1`; the release identity checker fails closed on
  dirty state, version/tag/revision mismatch, lightweight tags, invalid signatures, and unauthorized
  signer fingerprints.
- Focused release-preparation tests exercise semantic-version parsing, deterministic packaging,
  manifest/inventory inspection, checksum collision rejection, SBOM stability, and published-asset
  verification. Standard CI run `31423779827` passed the pre-tag release-preparation gate on
  Windows, Ubuntu, and macOS at main revision `3268874fd48ef57b584502c9830a02cdc885fc3b`.
- The Slice 5 foundation hardens that gate with static assertions over the candidate, proof,
  attestation, and publication workflow boundaries: release scope stays cooperative-only,
  read-only stages cannot tag or publish, and only the protected publication workflow may request
  `contents: write`.
- The signed tag, authorized signer trust root, exact-tag native candidate workflow, W-309 live
  walkthrough, isolated trusted-LAN rehearsal, reviewer attestation, and publication remain pending.
  No tag or GitHub Release is claimed here.
