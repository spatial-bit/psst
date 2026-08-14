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

## Start here

**Want two agents to talk to each other?** Begin with
[Start here: get two agents talking](docs/start-here.md). It gives one short path for one machine or
two Tailscale-connected machines and explains what Psst creates, where it keeps state, and which
commands stay running.

The direction is one public executable:

```text
psst relay start
psst --profile my-claude agent claude
psst --profile my-codex agent codex
```

The `agent` commands are the current Slice 7 implementation candidate and are not in the previously
verified alpha.2 archive. The package's bundled documentation is authoritative for its revision.

## Status

This is an **unreleased dogfood candidate**, not a production release. Follow the start page above
for the shortest package-appropriate route. The essential deployment distinction is always the
same: one machine hosts the relay; every other machine is a native client and must not start another
relay or copy credentials.
The [CLI reference](docs/cli-reference.md) records the exact low-level command surface, and
[development artifacts](docs/development-artifacts.md) describes verified short-retention dogfood
archives. Older manual tutorials remain available for troubleshooting and older packages, but are
no longer competing start pages. The separately controlled
[`v0.1.0-alpha.1` release process](docs/release-process.md) is prepared but remains unreleased:
signed-tag candidate builds, live Claude/Codex evidence, trusted-LAN rehearsal, independent
attestation, and publication approval are still required. Reviewed requirements are in
[PRD.md](PRD.md), and delivery gates are tracked in [ROADMAP.md](ROADMAP.md).

## Manual and legacy guides

The [agent-guided team setup runbook](docs/team-setup-agent-guide.md),
[Tailscale Jump Start](docs/jump-start-tailscale.md),
[two-agent push quickstart](docs/two-agent-push-quickstart.md), and
[Windows Codex + macOS Claude tutorial](docs/tutorial-windows-codex-macos-claude.md) document the
manual alpha.2-era setup and deeper diagnostics. Start with the single page above unless your
package predates `psst agent` or you need to inspect those lower-level steps.

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
