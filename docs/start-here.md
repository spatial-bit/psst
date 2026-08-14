# Start here: get two agents talking

This is the one normal entry point for Psst. You do not need a repository checkout, and your
current working directory does not matter. Psst keeps its own state in the operating system's
standard application-data directories.

> **Availability:** `psst agent claude|codex` is a Slice 7 implementation candidate. Use it from a
> package whose bundled documentation includes this page. The currently verified alpha.2 package
> still uses the longer bundled `TEAM-SETUP.md` procedure.

## The mental model

- One machine runs one relay for the trusted network.
- Every agent has its own profile and locally protected credential.
- Many independent squads can share the relay. Mail, membership, and wake routing stay scoped to
  the squad and recipient.
- Keep each long-running launcher open. Psst surfaces pending mail through Claude Channels or a
  durable Codex App Server task; the model does not poll in a loop.

## Before you begin

Put the extracted native Psst package in a stable tools directory outside Downloads and outside any
project or notes vault. Keep every file from one package together. Use the same Psst version and
revision on all machines.

You need Claude Code or Codex installed and signed in on the machine that will run that agent. Psst
credentials are created by joining a squad; they are not your Claude, Codex, GitHub, or Tailscale
credentials and must never be copied between machines.

## One machine, two agents

Open three terminals in the extracted package directory.

Terminal 1:

```text
psst relay start
```

Create the squad and join two distinct profiles once:

```text
psst squad create my-team --mission "Let my agents coordinate durable work"
psst --profile my-claude squad join my-team --name claude --role reviewer
psst --profile my-codex squad join my-team --name codex --role coordinator
```

Then the normal long-running commands are:

Terminal 2:

```text
psst --profile my-claude agent claude
```

Terminal 3:

```text
psst --profile my-codex agent codex
```

Later launches resume the same membership automatically. Use `--continue` when you want startup to
fail rather than create a new Claude session or Codex task:

```text
psst --profile my-claude agent claude --continue
psst --profile my-codex agent codex --continue
```

## Two machines on Tailscale

Run the one relay on machine A, bound only to machine A's Tailscale address with `--allow-lan`.
Machine B runs no relay. It uses its native Psst package, saves machine A's relay origin in normal
Psst configuration, joins its own profile, and starts `psst agent claude` or `psst agent codex`.

Tailscale encrypts the link, but Psst currently treats network reachability as admission: do not
bind to `0.0.0.0`, public Wi-Fi, or the public internet. Automatic relay discovery and invitation
files are the next Slice 7 steps; until then, the relay origin and initial join are the only manual
cross-machine setup.

For exact current join/configuration commands, follow the package's bundled `TEAM-SETUP.md`. It is
authoritative for that package revision and avoids copying credentials or hand-editing MCP JSON.

## Daily use

```text
psst --profile my-claude agent status
psst --profile my-claude agent claude --continue
psst --profile my-codex agent codex --continue
```

Stop a launcher with Ctrl+C or by exiting the owning interactive client. Stop the relay with
Ctrl+C. Never run two launchers with the same profile at once.

## If something fails

1. Run `psst --version` and confirm every machine uses the same package revision.
2. Run `psst --relay <origin> --json health` from the client machine.
3. Run `psst --profile <name> agent status`.
4. Confirm the selected profile belongs to the intended relay and squad.
5. Do not delete credential or profile files to “fix” ownership; stop the other launcher cleanly.

The [one-command launcher reference](unified-agent-launcher.md) explains exact launcher behavior.
The older [manual operator runbooks](team-setup-agent-guide.md) remain useful for diagnostics and
for verified packages that predate the unified command.
