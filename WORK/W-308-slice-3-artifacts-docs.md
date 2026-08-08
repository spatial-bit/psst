# W-308: Slice 3 dogfood artifacts and cooperative documentation

Status: blocked on W-305 and W-307

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

Pending.
