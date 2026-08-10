# Slice 3 interface contract

This document freezes the contract baseline implemented after ADR 0004. It does not claim that the
commands or MCP tools execute product behavior yet.

## Configuration and profile selection

Each non-secret field resolves independently in this order:

```text
command flag > environment variable > config file > safe default
```

The relay origin uses `--relay`, `PSST_RELAY`, `relay_origin`, then
`http://127.0.0.1:7341`. The profile uses `--profile`, `PSST_PROFILE`, `profile`, then
`default`. Relay bind uses `relay start --bind`, `PSST_RELAY_BIND`, `relay_bind`, then
`127.0.0.1:7341`; data directory uses `relay start --data-dir`, `PSST_DATA_DIR`, `data_dir`, then the
platform data directory; LAN permission uses `--allow-lan`, `PSST_ALLOW_LAN`, `allow_lan`, then
`false`; log level uses `--log`, `PSST_LOG`, `log_level`, then `info`; log format uses
`PSST_LOG_FORMAT`, `log_format`, then `text`. Message bytes use `PSST_MAX_MESSAGE_BYTES`,
`max_message_bytes`, then `65536` (allowed `1..65536` UTF-8 bytes); long poll uses `PSST_MAX_LONG_POLL_SECONDS`,
`max_long_poll_seconds`, then `30`; heartbeat uses `PSST_HEARTBEAT_SECONDS`,
`heartbeat_interval_seconds`, then `10` (allowed `1..300`); lease uses `PSST_LEASE_SECONDS`,
`lease_seconds`, then `30` (allowed `2..900` and strictly greater than heartbeat). Long poll is
allowed `0..30`. The typed contract preserves provenance for every field; W-302 enforces these bounds.
This is the domain message-content limit. The relay's larger HTTP request-body capacity is transport
overhead capacity and is not a configurable relaxation of the message limit.
No credential, authorization value, resume token, credential-file override, client mode,
sender identity, or dedupe key has an environment-variable or model-callable form.

A canonical relay origin contains only an HTTP or HTTPS scheme, host, and optional non-default port.
Canonicalization lowercases DNS names, removes a root trailing slash, and removes the scheme's
default port. It preserves the scheme and non-default port. User information, query, fragment, and
non-root paths are rejected. An override that differs from a bound profile's canonical origin fails
closed.

A profile is identified by `(canonical relay origin, local profile name)` and represents exactly one
durable squad membership. `squad_join` may choose the squad, member name, and role only while that
profile is unbound. Protected operations derive squad, sender, mailbox, mode, and authority from the
bound profile.

## Platform path roles

Paths are resolved through platform directory APIs, never the current directory:

| Role | Windows | macOS | Linux/Unix |
|---|---|---|---|
| Non-secret configuration | roaming application configuration directory | Application Support configuration directory | `$XDG_CONFIG_HOME`, falling back to `~/.config` |
| Profile metadata and secret | local application data directory | Application Support data directory | `$XDG_DATA_HOME`, falling back to `~/.local/share` |
| Lifetime profile lock | local runtime directory when available, otherwise profile data | local runtime directory when available, otherwise profile data | `$XDG_RUNTIME_DIR` when available, otherwise profile data |

The concrete application subdirectory is `psst`. Tests and explicit development overrides use
temporary directories. Non-secret configuration, non-secret profile metadata, and credential files
are separately versioned. The profile lock is held for the owning process lifetime. Credential files
use user-only access and atomic same-directory replacement; their exact format remains private to the
credential store.

## CLI output and process status

The frozen grammar is in `crates/psst-application/fixtures/cli-help.txt`. Human output is the default.
With `--json`, success writes exactly one `psst.cli.v1` envelope and newline to stdout with empty
stderr. Failure writes exactly one envelope and newline to stderr with empty stdout. There is no
surrounding prose. Every envelope identifies the command. Exit classes are `0` success, `2` usage,
`3` configuration, `4` unavailable, `5` conflict, `6` authority, `7` outcome unknown, `8` local I/O,
`9` local lock, and `70` internal. Effective configuration reports field-level provenance and only a
redacted local authentication-state enum; profile views expose binding identifiers but no private
authentication material.

Usage failures detected before an executable command can be selected use the failure-only command
identity `invocation`. It is never emitted by a success envelope and is not an executable command.

`relay start` is the long-running daemon exception to completion-oriented JSON output. After the
database is ready and the listener is bound, `--json` writes and flushes exactly one success envelope
whose data contains `running: true`, effective bind, database path, schema version, trusted-LAN state,
and the fixed no-TLS warning when applicable. Clean shutdown emits no second document. A fatal error
before startup uses the ordinary JSON failure envelope; a fatal error after the startup envelope uses
only a fixed non-JSON stderr diagnostic and nonzero status because a second JSON document would make
the one-document invocation contract ambiguous. A forced shutdown timeout retains the relay's bounded
immediate-exit behavior.

The command surface is frozen here, while implementation ownership is explicit: W-302 owns
configuration/profile commands; W-303 owns relay start and database commands; W-304 owns health;
W-305 owns squad/message/inbox/transcript/status CLI execution; W-306 owns MCP process/session
plumbing; W-307 owns MCP tool dispatch; W-308 owns listen and cooperative polling behavior.

## Cooperative MCP

`psst-mcp` is a stdio server. Stdout is protocol-only. One process owns one startup-selected profile.
The resolved SDK is crates.io `rmcp` exactly 3.1.2 with default features disabled and direct features
`server`, `macros`, and `transport-io`. Resolution adds the server-required `schemars`,
`transport-async-rw`, and `uuid` feature edges; no HTTP transport or client feature is enabled.
Psst frames both directions before rmcp: each complete JSON line, including newline, is at most
1,048,576 bytes. Oversized or unterminated input closes the session with exit 70 and one fixed stderr
diagnostic; input bytes are never reflected. Output is subject to the same bound. Pump/service
cancellation closes both in-process pipes; production shutdown orchestration is W-306 scope.
The nine full canonical tool schemas are checked in at
`crates/psst-application/fixtures/mcp-tools.snapshot.json`. Tool execution failures use the stable safe
error schema without process exit classes. JSON-RPC framing/method failures remain protocol errors;
completed tool calls report application failures through the tool error schema. Arbitrary remote
messages and details are not reflected.

All participant-controlled values remain values in explicitly untrusted structures. They are never
used as object keys or interpolated into compatibility prose. Compatibility content is canonical
JSON. Retrieval alone does not acknowledge; `message_receive.acknowledge_ids` is an explicit
mutation performed before retrieval.

Transport canaries prove hostile input is absent from both process streams. Content canaries prove
participant-controlled values remain JSON string values. These tests establish their named boundary,
not end-to-end prompt-injection resistance or future client behavior. Authentication canaries cover
MCP schema keys, descriptions, string values, defaults, examples, and compatibility text after
case/separator normalization. The deliberate allowlist is empty; process-only CLI configuration
views may expose the enum field `credential_state`, whose values reveal availability only.

The W-302 credential-store canary gate has one expected positive location: the single restricted
credential record owned by the selected profile. The canary must be absent from profile metadata,
configuration, lock files, atomic-write temporary files after completion or after recovery, logs,
stdout, stderr, test reports, packaged artifacts, and retained CI evidence. W-301 freezes this
evidence boundary but does not create the record or claim persistence evidence.
An abrupt process termination may leave one store-recognized same-directory replacement file. It
must already have the same user-only protection as the credential record before its first secret
byte is written, and store recovery must remove it before opening the profile for use. The crash
test may observe the canary only in that protected remnant before recovery and must verify its access
control before scanning again after recovery.

Structured cancellation covers the rmcp service task and both framing pumps: service startup failure
cancels and reaps both pumps; the first pump/service failure cancels and reaps its siblings; clean
input EOF permits a bounded service/output drain. It does not cover future relay calls, heartbeat,
long-poll cancellation, OS signal policy, or client activation, which belong to W-306 through W-308.

## Deferred behavior

This slice does not launch or control Claude or Codex, activate sessions, schedule turns, inject
terminal input, emit client-specific notifications, or implement a wake state machine. Those are
Slice 4 concerns.
