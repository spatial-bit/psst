# W-307: Cooperative MCP tools

Status: independently approved locally; native CI pending

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

- The nine frozen tools dispatch through one process-owned, startup-selected profile. Join fixes
  cooperative mode and internal MCP metadata; protected calls clone the active runtime under a
  short adapter lock and derive squad, sender, credential, heartbeat, resume, and retry identity
  from that runtime. Adapter locks never span relay, disk, heartbeat, availability, or long-poll
  work.
- `message_send` creates exactly one `PreparedSend` and hands it to the runtime's owned send
  ledger. `message_receive` completes its explicit bounded acknowledgement mutation before issuing
  the inbox read, while an empty acknowledgement list performs no mutation. Tool failures remain
  stable safe tool results; framing and invalid parameters remain JSON-RPC failures.
- Join and leave publication run in adapter-owned tasks. A deterministic real-relay test aborts
  each request waiter after the relay transaction reaches its durable terminal result but before
  adapter publication, immediately begins shutdown, and proves shutdown waits for publication,
  then either shuts the durable joined runtime or observes the completed leave. Metadata-absent
  startup reconciles the leave journal before treating a profile as unbound.
- A real relay test launches two independent `psst-mcp` child processes with isolated platform
  roots. It proves durable join, direct send, replay without implicit acknowledgement, explicit
  acknowledgement, canonical hostile delimiter/prompt-shaped content, adapter restart/resume,
  advisory availability, reply, and leave. A concurrent 30-second receive is canceled by leave and
  returns `invalid_session` while leave completes in the same bounded transcript. The test also
  forces an oversized-frame exit while the profile is bound, then immediately restarts that profile,
  proving every protocol exit path shuts down the runtime and releases ownership.
- The child test extracts the real restricted authorization record, recursively scans its complete
  profile root, and finds the authorization value only in the credential record before and after
  restart. Captured message/status structured and compatibility results exclude it. The frozen MCP
  schema scan contains no credential, authorization, resume, dedupe, sender, mode, or client
  selectors; the only heartbeat references are sanitized status output and security description.
- Windows focused evidence: all 11 `psst-mcp` tests pass (six unit, four bounded child-stdio, one
  real-relay/two-child transcript); strict Clippy passes for every `psst-mcp` and
  `psst-application` target/feature; focused formatting and `git diff --check` pass. All child
  processes use isolated platform roots and profiles, so ambient operator state cannot affect the
  handshake suite.
- Successful runtime shutdown releases profile ownership only after reports, sends, scheduler, and
  the lifecycle operation gate have drained. Timeout retains ownership. A deterministic blocked
  leave test denies a competing owner throughout shutdown, then proves terminal cleanup, lock
  release, and survival of a new owner's sentinel metadata.
- Coordinator Windows gate: serialized `cargo test --workspace --all-targets --all-features
  --locked -- --test-threads=1`, strict workspace Clippy with warnings denied, workspace formatting,
  and diff integrity all pass. Windows/Linux/macOS CI remains pending, so cross-platform
  verification is not yet claimed.
- Independent adversarial review initially rejected cancellable adapter publication, transition-time
  shutdown, and metadata-absent leave recovery. After the owned-publication, bounded-settlement, and
  startup-reconciliation repairs plus deterministic regressions, the fresh review approved W-307.
  A final ownership re-review also approved operation-gate draining before profile-lock release.
