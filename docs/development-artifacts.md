# Development artifacts

CI produces native **unreleased dogfood builds** of the Psst CLI, cooperative MCP adapter, and
transitional relay. They are not releases or production-ready packages.

## Platforms and names

| Runner | Target label | Archive |
|---|---|---|
| Windows x86-64 | `windows-x86_64` | `psst-dogfood-<version>-<revision>-windows-x86_64.zip` |
| Linux x86-64 | `linux-x86_64` | `psst-dogfood-<version>-<revision>-linux-x86_64.tar.gz` |
| macOS ARM64 | `macos-aarch64` | `psst-dogfood-<version>-<revision>-macos-aarch64.tar.gz` |

`<version>` is the Cargo package version and `<revision>` is the full Git commit SHA built by
GitHub Actions. The archive has one same-named root directory containing exactly:

- `psst`, `psst-mcp`, and transitional `psst-relay` (`.exe` on Windows);
- `LICENSE`;
- `BUILD-INFO.txt`, with artifact name, target label, version, and revision;
- `DEVELOPMENT-BUILD`, with the unreleased dogfood and trusted-LAN warning;
- `DOGFOOD-QUICKSTART.md`, the concise local, LAN, CLI, profile, delivery, and MCP guide.

Archive paths, ordering, timestamps, ownership metadata, and permissions are normalized. This makes
the packaging layout predictable; it does not claim the compiler output or archives are
reproducible. CI rejects extra paths, unsafe members, incorrect executable modes, workspace/secret
canaries, wrong versions, and incomplete warnings. Native jobs smoke the relay, CLI configuration,
and MCP initialize handshake. Clean-download jobs repeat exact inventory, canary, version, relay,
CLI, and MCP checks using only the downloaded archive and operating-system tools—no Rust toolchain
or repository checkout.

## Download and dogfood

Open a successful Development artifacts workflow run for the desired revision and download its
clearly labelled `psst-dogfood-...` artifact. GitHub wraps the retained native archive in an outer
download ZIP, so extract that wrapper and then the inner `.zip` or `.tar.gz`. Read
`DEVELOPMENT-BUILD`, then follow `DOGFOOD-QUICKSTART.md` from the extracted directory.

The supported cooperative surface in this archive is voluntary CLI and standard MCP tool use for
generic hosts, Claude Code, and Codex. One adapter process owns one selected profile; credentials,
heartbeat, resume, sender identity, mode, and dedupe identity remain internal. Retrieval is replayed
until explicit acknowledgement.

The relay has no TLS and must not be exposed to the internet. Non-loopback binding is only for a
trusted LAN and requires explicit opt-in. Keep credentials in their protected platform profile
directory and out of logs, shared folders, prompts, and bug reports.

Scheduling, Claude Channels, Codex App Server activation, keystroke injection, installers, formal
checksum manifests, SBOMs, signed tags, and GitHub Releases are explicitly deferred. These archives
remain unsigned, short-retention development artifacts with no compatibility or support promise.
