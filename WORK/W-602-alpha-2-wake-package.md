# W-602: Alpha.2 wake-harness package

Status: local candidate; native package CI pending

## Objective

Define and build portable `v0.1.0-alpha.2` dogfood archives that include the verified Claude Channel
and Codex App Server wake harnesses rather than only cooperative MCP.

## Acceptance

- Exact archive inventory and compatibility requirements are explicit.
- `psst-codex` and Channel-capable `psst-mcp` are built, inspected, and smoke-tested natively.
- Existing deterministic manifests, checksums, SBOM, path safety, and no-secret guarantees remain.
- Unsupported host versions fail closed with actionable diagnostics.

## Evidence

- Workspace version and every workspace package are `0.1.0-alpha.2` under the locked dependency
  graph.
- Development archives now contain the four runtime binaries, internal SHA-256 manifest, SPDX
  SBOM, build metadata, license, and quickstart. A separate retained SHA-256 file authenticates the
  archive before extraction.
- The inspector verifies exact inventory, safe paths and modes, manifest byte counts and hashes,
  SBOM identity, build identity, and decompressed secret/path canaries.
- Local deterministic packaging tests, strict workspace Clippy, formatting, diff checks, and the
  complete workspace test suite pass. Native and checkoutless workflow evidence remains pending.
