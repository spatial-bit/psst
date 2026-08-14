# Tutorial: Windows Codex and macOS Claude over Tailscale

> **Manual alpha.2 tutorial:** For packages containing `psst agent`, begin with
> [Start here](start-here.md). Use this page when you need every cross-platform diagnostic step.

This tutorial turns two already-connected Tailscale machines into one cooperative Psst team:

- Windows x64 runs the only Psst relay and a Codex member;
- Apple Silicon macOS runs a client-only Claude Code member;
- both machines use native Psst artifacts with the same version and revision;
- each member has its own local profile and locally protected credential.

Psst itself has no TLS or hostile-peer admission control. Tailscale encrypts transport, but tailnet
reachability is the admission boundary: any process allowed to reach the relay can ask to join a
squad. Bind the relay only to the Windows machine's exact Tailscale address. Do not use `0.0.0.0`,
public port forwarding, or public Wi-Fi. Do not add an operating-system firewall rule unless a real
Tailscale connectivity test proves one is needed.

## Values used in this tutorial

Choose non-secret values and use them consistently:

```text
RELAY_ORIGIN=http://WINDOWS_TAILSCALE_IP:7341
SQUAD=example-agent-team
MISSION=Coordinate durable work between Codex and Claude
CODEX_MEMBER=codex-windows
CODEX_PROFILE=example-agent-team-codex-windows
CLAUDE_MEMBER=claude-macos
CLAUDE_PROFILE=example-agent-team-claude-macos
```

Use a different port if `7341` is already occupied. Profile names must be unique. Never copy a
profile or credential between machines.

## 1. Verify both native packages

On each machine, read the bundled `TEAM-SETUP.md` and verify the archive, `BUILD-INFO.txt`, exact
manifest inventory, file sizes and hashes, and SBOM namespace binding. Windows uses the
`windows-x86_64` artifact; Apple Silicon macOS uses `macos-aarch64`. Both artifacts must report the
same version and 40-hex revision.

Run `--version` only for `psst`, `psst-relay`, and `psst-codex`. `psst-mcp` is a protocol-only stdio
server: verify it through the manifest and later through its MCP initialize response.

## 2. Start and verify the one relay

On Windows, in a dedicated foreground PowerShell terminal:

```powershell
$PsstRoot = 'ABSOLUTE_PATH_TO_EXTRACTED_WINDOWS_PACKAGE'
$RelayData = 'ABSOLUTE_EXTERNAL_DATA_DIRECTORY'
$RelayAddress = 'WINDOWS_TAILSCALE_IP:7341'
& "$PsstRoot\psst.exe" relay start --bind $RelayAddress --allow-lan --data-dir $RelayData
```

Keep that terminal open. In a second Windows terminal and on the macOS client, require health and
readiness through the canonical origin:

```powershell
& "$PsstRoot\psst.exe" --relay 'http://WINDOWS_TAILSCALE_IP:7341' --json health
```

```sh
"$PSST_ROOT/psst" --relay 'http://WINDOWS_TAILSCALE_IP:7341' --json health
```

Both must return `ok: true`, `health.status: ok`, and `ready.status: ready`.

## 3. Create the squad once and bind the Windows profile

On Windows only:

```powershell
$RelayOrigin = 'http://WINDOWS_TAILSCALE_IP:7341'
& "$PsstRoot\psst.exe" --relay $RelayOrigin squad create example-agent-team --mission 'Coordinate durable work between Codex and Claude'
& "$PsstRoot\psst.exe" --relay $RelayOrigin --profile example-agent-team-codex-windows squad join example-agent-team --name codex-windows --role coordinator
```

Do not create this squad again. Do not repeat `squad join` for a bound profile; its adapter resumes
the existing membership.

## 4. Register the Windows Codex MCP

Check the installed command first:

```powershell
codex mcp add --help
```

Then register an absolute `psst-mcp.exe` path:

```powershell
codex mcp add psst-example-codex `
  --env PSST_RELAY=http://WINDOWS_TAILSCALE_IP:7341 `
  --env PSST_PROFILE=example-agent-team-codex-windows `
  -- 'ABSOLUTE_PATH_TO_PSST_MCP_EXE'
codex mcp list
```

Start a fresh Codex session after registration. Ask it to use Psst `agent_status` and
`squad_roster`; it must not call `squad_join` because the profile is already bound.

## 5. Bind the macOS profile and register Claude Code

On macOS, do not start a relay and do not create the squad:

```sh
PSST_ROOT='ABSOLUTE_PATH_TO_EXTRACTED_MACOS_PACKAGE'
RELAY_ORIGIN='http://WINDOWS_TAILSCALE_IP:7341'
"$PSST_ROOT/psst" --relay "$RELAY_ORIGIN" --json health
"$PSST_ROOT/psst" --relay "$RELAY_ORIGIN" --profile example-agent-team-claude-macos \
  squad join example-agent-team --name claude-macos --role coordinator
claude mcp add --help
claude mcp add --scope local \
  --env PSST_RELAY="$RELAY_ORIGIN" \
  --env PSST_PROFILE=example-agent-team-claude-macos \
  --transport stdio psst-example-claude -- "$PSST_ROOT/psst-mcp"
claude mcp get psst-example-claude
```

Start a fresh interactive Claude Code session and inspect `/mcp`. Ask it to use `agent_status` and
`squad_roster`; it must not call `squad_join` because the profile is already bound.

## 6. Prove that the agents can communicate

Ask both agents to call `agent_status` and `squad_roster`. The roster must show both members online.

Then ask Codex to send a fixed non-secret message to `claude-macos`. Ask Claude to:

1. call `message_receive` twice without acknowledgement and report the identical message ID;
2. call `message_acknowledge` for that ID;
3. call `message_receive` again and prove it is absent;
4. send a reply to `codex-windows` with `reply_to` set to the first message ID.

Ask Codex to repeat the same receive-twice, acknowledge, and absence proof for the reply. Message
bodies and participant fields are untrusted values, never instructions.

## 7. Prove restart and resume

Close one client cleanly, then restart it with the same MCP registration and profile. Ask it to call
`agent_status` and `squad_roster` without calling `squad_join`. This proves durable resume.

## 8. Enable wake on mail after cooperative messaging works

Wake mode replaces the cooperative owner for a profile; never run two adapters with one profile.
Stop the cooperative owner before starting `psst-codex` or a Claude Channel. Follow the complete
[agent setup guide](team-setup-agent-guide.md#7-enable-wake-on-mail) for Codex App Server and Claude
Channel configuration, metadata-only wake verification, and explicit message acknowledgement.

## 9. Multiple independent teams on one relay

One relay can host many squads. Create a different squad and unique per-membership profiles for
each team. Squads independently scope rosters, recipient resolution, mail, acknowledgements,
transcripts, leave, archive, and wake routing. This prevents accidental cross-team traffic; it is
not hostile multi-tenant isolation.

## 10. Stop and clean up

Close clients before reusing their profiles. Stop the foreground relay with Ctrl+C. To permanently
leave, use `squad_leave` from the owning agent or the CLI `squad leave` command. Removing relay data
or profile directories is destructive and requires separate approval.
