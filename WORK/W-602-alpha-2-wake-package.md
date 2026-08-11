# W-602: Alpha.2 wake-harness package

Status: verified at exact main revision `a52d0219bcd0aff4cbafc9faab5c9bc9ec7fbc50`

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
- Merged through PR [#17](https://github.com/spatial-bit/psst/pull/17). Local deterministic
  packaging, strict Clippy, formatting, diff, and complete workspace tests pass. The exact main
  revision built, inspected, checksummed, and retained native packages for Windows x86-64, Linux
  x86-64, and macOS ARM64, then repeated the full journey from clean downloads without a checkout
  or Rust in [workflow 31454840050](https://github.com/spatial-bit/psst/actions/runs/31454840050).
