# Psst

Psst is a featherweight LAN messaging and activation substrate for autonomous AI agent squads.

The project is in its foundation phase. Its intended command vocabulary includes:

```text
psst inbox
psst listen
psst send
psst roster
psst squad
```

Psst will provide durable direct messages, squad membership, leased presence, and thin adapters for cooperative and harnessed Claude Code and Codex sessions.

## Status

The implementation is not yet usable. The reviewed requirements are in [PRD.md](PRD.md), and delivery gates are tracked in [ROADMAP.md](ROADMAP.md).

## Security boundary

Version one assumes a trusted LAN. It is not designed for hostile networks, public Wi-Fi, internet exposure, or multi-tenant deployment. See [SECURITY.md](SECURITY.md).

## Development

Requires Rust 1.89 or later.

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

MIT

