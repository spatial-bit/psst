# W-306: Cooperative MCP transport and schema contract

Status: verified

## Objective

Implement a bounded, protocol-pure `psst-mcp` stdio server shell and the reviewed nine-tool schema without Psst business execution.

## Dependencies

- W-301.

## Acceptance

- Initialize/version negotiation, initialized notification, ping, tools/list, tools/call, cancellation, EOF, and clean shutdown interoperate with independent MCP host drivers.
- Stdout contains MCP frames only; serialized writes prevent interleaving and all diagnostics use stderr.
- Input/frame/output sizes are bounded; malformed, oversized, unknown method/tool, invalid params, cancellation, and shutdown cases are deterministic.
- Golden schemas cover exactly the nine PRD tools, reject unknown/secret-shaped fields, and include reviewed annotations and security instructions.
- Tool execution errors use bounded secret-safe tool results; protocol errors remain JSON-RPC errors.
- Static and runtime scans prove no Claude/Codex launch, `claude -p`, Channels, App Server, wake, or host-control path.

## Verification evidence

- `psst-mcp` now implements initialize/version negotiation, initialized notification handling, ping,
  exact nine-tool discovery, validated invocation routing, cancellation notification handling, and
  bounded EOF shutdown through crates.io `rmcp` 3.1.2.
- Known tools return a fixed `unsupported` tool-level result with `isError: true`, canonical JSON
  text, and matching structured content until W-307 supplies business dispatch. Because
  `tools/call` is a known method, an unadvertised tool name and malformed tool arguments are both
  invalid method parameters (`-32602`), not unknown methods (`-32601`).
- Five unit tests prove exact frozen schema/annotation/error metadata conversion, closed runtime
  argument validation and UTF-8 bounds (including ECMAScript/C1 correlation boundaries),
  initialization metadata, the complete framing matrix, and structured in-flight cancellation.
  Four child-process tests prove the independent stdio transcript, cancellation/ping continuity,
  malformed and oversized fail-closed behavior, stdout purity, bounded cleanup, and clean EOF. An
  injectable in-process blocking worker is owned by an abort-on-drop handle, and proves request
  cancellation stops and joins genuinely in-flight work while leaving the same MCP session usable
  for a subsequent ping. The bounded test starts client/server negotiation concurrently, owns both
  interaction and service tasks, and reaps both on success, panic, or timeout.
- `cargo fmt --all -- --check` and strict workspace/all-target Clippy pass. All 190 workspace tests
  pass on Windows; the credential permission tests require the normal user token because the sandbox
  token is intentionally excluded after ACL restriction.
- Independent adversarial review approved the exact schema conversion, protocol/tool error boundary,
  Unicode validator parity, structured cancellation, and bounded task cleanup.
- Revision `1617ec5` passed GitHub Actions on Windows, Linux, and macOS plus native Windows x86-64,
  Linux x86-64, and macOS ARM64 development-artifact builds:
  <https://github.com/spatial-bit/psst/actions/runs/31249745457> and
  <https://github.com/spatial-bit/psst/actions/runs/31249745452>.
