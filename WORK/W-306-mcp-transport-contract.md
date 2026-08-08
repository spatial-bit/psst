# W-306: Cooperative MCP transport and schema contract

Status: blocked on W-301

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

Pending.
