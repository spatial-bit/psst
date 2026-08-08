# W-307: Cooperative MCP tools

Status: blocked on W-303 and W-306

## Objective

Connect the reviewed MCP tools to the shared session runtime and typed client for cooperative Claude Code and Codex use.

## Dependencies

- W-303 and W-306.

## Acceptance

- All nine tools map exactly to the active profile and typed client; model arguments cannot select credentials, sender identity, mode, client metadata, resume, heartbeat, or dedupe key.
- Join persists before returning; leave ambiguity retains local authority; send uses one prepared operation; receive acknowledges explicit prior IDs before retrieval and retrieval alone never acknowledges.
- Message results contain fixed security notice plus structured `trust: untrusted_participant_content` and `untrusted_body`; canonical compatibility text survives delimiter/prompt-injection-shaped bodies.
- Heartbeat/resume continue without tool calls and `agent_status` exposes only sanitized state and advisory availability.
- Real relay lifecycle/replay/ack/reconnect tests, child-process stdio transcripts, injection corpus, and recursive credential-canary scans pass on all platforms.

## Verification evidence

Pending.
