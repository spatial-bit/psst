# Development artifacts

CI produces native **unreleased dogfood builds** of the Psst CLI, cooperative MCP adapter, Codex
App Server wake harness, and transitional relay. They are not releases or production-ready
packages.

## Platforms and names

| Runner | Target label | Archive |
|---|---|---|
| Windows x86-64 | `windows-x86_64` | `psst-dogfood-<version>-<revision>-windows-x86_64.zip` |
| Linux x86-64 | `linux-x86_64` | `psst-dogfood-<version>-<revision>-linux-x86_64.tar.gz` |
| macOS ARM64 | `macos-aarch64` | `psst-dogfood-<version>-<revision>-macos-aarch64.tar.gz` |

`<version>` is the Cargo package version and `<revision>` is the full Git commit SHA built by
GitHub Actions. The archive has one same-named root directory containing exactly:

- `psst`, `psst-mcp`, `psst-codex`, and transitional `psst-relay` (`.exe` on Windows);
- `LICENSE`;
- `BUILD-INFO.txt`, with artifact name, target label, version, and revision;
- `DEVELOPMENT-BUILD`, with the unreleased dogfood and trusted-LAN warning;
- `DOGFOOD-QUICKSTART.md`, the concise local, LAN, CLI, profile, delivery, and MCP guide.
- `TEAM-SETUP.md`, the complete agent-readable one-hub/multi-team setup, wake, verification,
  troubleshooting, and cleanup runbook;
- `MANIFEST.json`, with the exact internal inventory, sizes, and SHA-256 hashes;
- `SBOM.spdx.json`, the deterministic SPDX 2.3 package inventory.

The retained workflow artifact also contains `<target>.SHA256`, which hashes the inner native
archive before extraction. This remains an unsigned development checksum, not a signature.

Archive paths, ordering, timestamps, ownership metadata, and permissions are normalized. Repacking
the same inputs is deterministic; compiler reproducibility is not claimed. CI rejects extra paths,
unsafe members, incorrect executable modes, manifest or checksum mismatches, malformed SBOMs,
workspace/secret canaries, wrong versions, and incomplete warnings. Native jobs smoke the relay,
CLI configuration, MCP initialize handshake, and complete Claude and Codex wake cycles. The Claude
leg proves a
body-free Channel wake, replay before explicit acknowledgement, no duplicate wake, and restart
reconciliation. The Codex leg proves idle-before-send, real relay mail, dynamic receive and
explicit acknowledgement through the built `psst-mcp`, one turn, zero pending mail, and clean
shutdown. A second squad reuses the same Claude and Codex member names; its decoy mail must produce
zero notification or turn in the first squad and remain pending only for its own recipients.
Clean-download jobs repeat exact inventory, canary, version, relay, CLI, MCP, and wake checks using
only the downloaded archive, a separately retained test harness, and operating-system tools—no Rust
toolchain or repository checkout.

## Download and dogfood

Open a successful Development artifacts workflow run for the desired revision and download its
clearly labelled `psst-dogfood-...` artifact. GitHub wraps the retained native archive in an outer
download ZIP, so extract that wrapper and then the inner `.zip` or `.tar.gz`. Read
`DEVELOPMENT-BUILD`, then follow `DOGFOOD-QUICKSTART.md` from the extracted directory.

The cooperative surface in this archive includes voluntary CLI and standard MCP tool use for
generic hosts, Claude Code, and Codex. It also includes the opt-in experimental Claude Channel and
Codex App Server wake adapters documented in the bundled quickstart. One adapter process owns one
selected profile; credentials, heartbeat, resume, sender identity, mode, and dedupe identity remain
internal. Retrieval is replayed until explicit acknowledgement.

The relay has no TLS and must not be exposed to the internet. Non-loopback binding is only for a
trusted LAN and requires explicit opt-in. Keep credentials in their protected platform profile
directory and out of logs, shared folders, prompts, and bug reports.

Scheduling, keystroke injection, installers, signatures, signed tags, and GitHub Releases are
explicitly deferred. The checksum and SBOM establish artifact integrity and inventory but do not
authenticate a publisher. The wake adapters remain experimental preview surfaces.
These archives remain unsigned, short-retention development artifacts with no compatibility or
support promise.
