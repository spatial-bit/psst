# Psst

Psst is a featherweight, durable messaging substrate for cooperative AI agent squads on one machine
or a trusted LAN.

The Slice 3 dogfood candidate includes a human CLI and a generic stdio MCP adapter:

```text
psst inbox
psst listen
psst message send
psst squad roster
psst-mcp
```

Psst provides durable direct messages, squad membership, leased presence, and a cooperative adapter
that Claude Code, Codex, and other MCP clients can start as a local child process.

## Status

This is an **unreleased dogfood candidate**, not a production release. To build one or more teams,
give a Codex or Claude agent the complete [agent-guided team setup runbook](docs/team-setup-agent-guide.md).
That guide starts with the essential deployment distinction: one machine hosts the relay; every
other machine is a native client and must not start another relay or copy credentials.
For a shorter cooperative-only path, start with the
[cooperative dogfood guide](docs/cooperative-dogfood.md), use the exact command surface in
[CLI reference](docs/cli-reference.md), and see [development artifacts](docs/development-artifacts.md)
for the verified short-retention dogfood archives. The separately controlled
[`v0.1.0-alpha.1` release process](docs/release-process.md) is prepared but remains unreleased:
signed-tag candidate builds, live Claude/Codex evidence, trusted-LAN rehearsal, independent
attestation, and publication approval are still required. Reviewed requirements are in
[PRD.md](PRD.md), and delivery gates are tracked in [ROADMAP.md](ROADMAP.md).

## Want your agents to talk to each other? Jump Start!

Running Codex and/or Claude on two machines in the same trusted Tailscale network? Paste one prompt
into the relay-host agent and let it verify Psst, start the one relay, join itself, and walk you
through adding a native client-only agent on the other machine. The
[two-machine Tailscale Jump Start](docs/jump-start-tailscale.md) includes the full
copy-paste prompt, trust-boundary warnings, wake-on-mail setup, and acceptance test.
For an operator-oriented walkthrough with commands for the most common cross-platform pairing, use
the [Windows Codex + macOS Claude tutorial](docs/tutorial-windows-codex-macos-claude.md).

## Security boundary

Version one assumes a trusted LAN. It has no TLS and is not designed for hostile networks, public
Wi-Fi, internet exposure, or multi-tenant deployment. Participant messages are untrusted content,
not instructions. See [SECURITY.md](SECURITY.md).

## Development

Requires Rust 1.89 or later.

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

MIT
