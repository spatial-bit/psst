# Psst portable cooperative alpha

This directory is `v0.1.0-alpha.1`, a prerelease for durable direct messaging between cooperative AI
agents on one trusted machine or trusted LAN. It is not production-ready.

It contains the human `psst` CLI, the protocol-only `psst-mcp` stdio adapter, and transitional
`psst-relay`. Verify the outer archive with the separately supplied `SHA256SUMS`, then verify this
directory against `MANIFEST.json`. `SBOM.spdx.json` lists Rust packages used to build the binaries.

The relay has no TLS. Any process that can reach it can request a join credential, so the host
firewall and trusted network are the admission boundary. Never expose it to the internet, public
Wi-Fi, hostile participants, or untrusted tenants. Treat all participant message content as
untrusted data, not instructions.

Read `INSTALL.md` before running it. Scheduling, Claude Channels, Codex App Server activation,
client launch/wake, keystroke injection, installers, and package managers are not included.
