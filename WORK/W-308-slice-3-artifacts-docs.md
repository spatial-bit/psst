# W-308: Slice 3 dogfood artifacts and cooperative documentation

Status: verified at c2e4dad as unreleased Slice 3 dogfood artifacts and documentation; not a release or production-readiness gate

## Objective

Extend unreleased native dogfood artifacts and documentation to cover the `psst` CLI and `psst-mcp` cooperative adapter.

## Dependencies

- W-305 and W-307.

## Acceptance

- Windows x86-64, Linux x86-64, and macOS ARM64 development archives contain reviewed `psst`, `psst-mcp`, transitional relay binary if retained, license, revision metadata, warning, and concise quickstart.
- Native and clean-download jobs inspect exact inventory/modes/path and secret canaries, then run version, relay, CLI, and MCP handshake smoke without Rust.
- Docs cover local and trusted-LAN setup, CLI reference, profiles/credential behavior, delivery/ack semantics, generic MCP, Claude cooperative, Codex cooperative, troubleshooting, and security limits.
- Documentation commands and links are executable/checked and explicitly defer scheduling, Channels, App Server, keystroke injection, installers, checksums, SBOMs, signed tags, and Releases.

## Verification evidence

- README now points dogfood users to the cooperative guide, CLI reference, and artifact status.
- Local/trusted-LAN, profiles/credentials, durable delivery/acknowledgement, generic MCP, Claude
  cooperative, Codex cooperative, troubleshooting, security limits, and Slice 4/5 deferrals are
  documented against the checked-in CLI help and MCP contracts.
- The native matrix packages `psst`, `psst-mcp`, and `psst-relay` with the license, revision metadata,
  warning, and quickstart. Native and clean-download jobs enforce exact inventory and modes, reject
  unsafe member paths before extraction, scan decompressed members in ASCII and UTF-16LE, and smoke
  version, relay health/readiness, live CLI health, isolated CLI configuration, and MCP initialize.
- A remapped Windows release build passed relay and cooperative CLI/MCP smoke plus exact ZIP inventory,
  mode, revision, warning, and recursive payload-canary inspection. Python syntax and diff checks pass.
- Independent review approved the implementation after the recursive payload scans, exact
  clean-download checks, live CLI assertions, dynamic loopback port, and pre-extraction path checks
  were repaired. These remain unreleased dogfood archives, not a production Release.
- The reviewed revision `c2e4dad3adf67b271a1b6a03a700711818e81503` passed the complete manually
  dispatched native workflow: [run 31272964987](https://github.com/spatial-bit/psst/actions/runs/31272964987).
  Native builds passed for [Windows x86-64](https://github.com/spatial-bit/psst/actions/runs/31272964987/job/93141925172),
  [Linux x86-64](https://github.com/spatial-bit/psst/actions/runs/31272964987/job/93141925145), and
  [macOS ARM64](https://github.com/spatial-bit/psst/actions/runs/31272964987/job/93141925135).
  Checkoutless clean-download rehearsals passed for
  [Windows](https://github.com/spatial-bit/psst/actions/runs/31272964987/job/93142403957),
  [Linux](https://github.com/spatial-bit/psst/actions/runs/31272964987/job/93142403954), and
  [macOS](https://github.com/spatial-bit/psst/actions/runs/31272964987/job/93142403964).
- These are unsigned, short-retention development artifacts for only the three evidenced target
  architectures. Installers, formal checksums, SBOMs, signatures and signed tags, GitHub Releases,
  scheduling/activation, Channels, App Server, and keystroke injection remain explicitly deferred.
