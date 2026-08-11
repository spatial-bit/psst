# W-602: Alpha.2 wake-harness package

Status: planned

## Objective

Define and build portable `v0.1.0-alpha.2` dogfood archives that include the verified Claude Channel
and Codex App Server wake harnesses rather than only cooperative MCP.

## Acceptance

- Exact archive inventory and compatibility requirements are explicit.
- `psst-codex` and Channel-capable `psst-mcp` are built, inspected, and smoke-tested natively.
- Existing deterministic manifests, checksums, SBOM, path safety, and no-secret guarantees remain.
- Unsupported host versions fail closed with actionable diagnostics.
