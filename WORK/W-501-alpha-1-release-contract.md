# W-501: v0.1.0-alpha.1 release contract

Status: candidate defined; independent approval pending

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

Pending independent review and candidate CI.
