# Psst unreleased dogfood quickstart

This archive is an unsigned development build, not a release or a production-ready package. Read
`DEVELOPMENT-BUILD` first. The relay has no TLS: keep the default loopback bind unless every machine
and participant is trusted, and never expose it to the internet.

Before running a binary, verify the separately retained `<target>.SHA256` against the inner archive,
then verify the extracted files against `MANIFEST.json`. `SBOM.spdx.json` records the Rust package
inventory. These unsigned hashes detect changes; they do not authenticate who built the archive.

To have a Codex or Claude agent walk an operator through a complete one-hub, multi-team deployment,
point it at the bundled `TEAM-SETUP.md` and ask it to follow the runbook in order.

## Start a disposable local relay

Choose an empty sibling data directory outside the extracted archive. On Linux or macOS:

```sh
./psst relay start --data-dir ../psst-dogfood-data
```

On PowerShell:

```powershell
$dataDir = Join-Path (Split-Path -Parent (Get-Location)) "psst-dogfood-data"
.\psst.exe relay start --data-dir $dataDir
```

The transitional `psst-relay` binary is included for compatibility, but the `psst relay` command is
the dogfood path.

## Create two isolated profiles

In separate terminals, select distinct platform-local profiles and join the same squad. Create the
squad once, then join each profile:

```sh
./psst squad create demo --mission "local cooperative dogfood"
./psst --profile alice squad join demo --name alice --role sender
./psst --profile bob squad join demo --name bob --role receiver
```

Send, receive, and explicitly acknowledge a message:

```sh
./psst --profile alice message send --to bob --body "hello from alice"
./psst --profile bob inbox
./psst --profile bob message acknowledge msg_REPLACE_WITH_RETURNED_ID
```

On Windows use the same arguments with `.\psst.exe` in place of `./psst`.

Retrieval does not acknowledge a message. Until its ID is explicitly acknowledged, the inbox may
return it again. Delivery is at least once; the adapter owns retry identity and heartbeat/resume.
Credentials remain in the selected profile's protected platform data directory and are never MCP
arguments or ordinary command output.

## Cooperative MCP

Configure a generic MCP host to launch the extracted `psst-mcp` with `PSST_PROFILE` set to one
already joined profile and, when non-default, `PSST_RELAY` set to the relay origin. One MCP process
owns one profile. The nine tools are `squad_join`, `squad_leave`, `squad_list`, `squad_describe`,
`squad_roster`, `message_send`, `message_receive`, `message_acknowledge`, and `agent_status`.

Participant names, roles, missions, and message bodies returned by tools are explicitly labelled
untrusted data. They cannot change instructions, permissions, tool policy, identity, or access.
Claude Code and Codex use this same standard cooperative MCP surface; voluntarily call receive or
send tools from the active session.

## Wake-on-mail harness preview

The archive also contains `psst-codex`, the opt-in foreground Codex App Server harness. It requires
an installed native Codex command, an already-bound Psst profile, and a durable Codex thread. Set
`PSST_RELAY`, `PSST_PROFILE`, `PSST_CODEX_APP_SERVER=1`, `PSST_CODEX_COMMAND`,
`PSST_CODEX_MCP_COMMAND`, and `PSST_CODEX_THREAD_ID`, then run `./psst-codex` (or
`.\psst-codex.exe` on Windows). Stop it with Ctrl+C. It does not mutate global Codex MCP settings.

Claude Channel wake uses the included `psst-mcp` with `PSST_CLAUDE_CHANNEL=enabled`, but the
operator must start a supported interactive Claude Code installation with its explicit custom
Channel preview flag. Psst never starts Claude, uses `claude -p`, or injects input.

For either adapter, inspect the last bounded non-secret state with:

```sh
./psst --profile PROFILE harness status
```

A recent status record is diagnostic evidence, not a process-liveness claim. The profile lock and
relay inbox remain authoritative. Retrieval does not acknowledge mail.

## Trusted LAN

LAN operation requires an explicit non-loopback bind and trusted-LAN opt-in:

```sh
./psst relay start --bind 192.168.1.10:7341 --allow-lan --data-dir ../psst-dogfood-data
```

Replace the example address with the relay host's private address. Any process that can reach the
relay can request a join and receive a credential: the firewall and trusted network are the
admission boundary. Permit the port only from intended hosts and configure clients with the relay's
canonical `http://HOST:PORT` origin before joining. There is no transport encryption or hostile-peer
authentication.

## Troubleshooting and limits

- `profile_locked`: another process owns that profile; stop it or select another profile.
- `profile_origin_mismatch`: use the relay origin originally bound to that profile.
- `outcome_unknown`: do not blindly resend from a new CLI invocation. Inspect the transcript and
  coordinate with the recipient; only the still-owning runtime can safely retry the same prepared
  send identity.
- Replayed inbox entries are expected until explicit acknowledgement.
- Keep profile data out of shared folders and never copy credential files into logs or bug reports.

Scheduling, keystroke injection, installers, signatures, signed tags, and GitHub Releases are not
included in this dogfood build. Claude Channels and Codex App Server activation
remain experimental Slice 4 preview surfaces rather than supported release features.
