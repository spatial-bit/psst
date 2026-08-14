# One-command agent launcher

Status: Slice 7 implementation candidate. This command surface is not in the verified alpha.2
artifact yet.

Psst is converging on one normal executable. Operator commands, relay startup, and long-running
agent harnesses use `psst`; client-specific and protocol-only process roles stay internal.

```text
psst relay start
psst --profile research-claude agent claude
psst --profile research-codex agent codex
```

The existing `psst-mcp`, `psst-codex`, and `psst-relay` executables remain temporary compatibility
shims during native migration. New generated configuration invokes the absolute current executable
as `psst internal mcp`. That internal command is deliberately absent from normal help and its stdout
is MCP protocol only.

## Agent state and working directory

Psst resolves its normal platform directories regardless of the shell's current directory:

- Windows configuration: `%APPDATA%\psst`; state: `%LOCALAPPDATA%\psst`;
- macOS: `$HOME/Library/Application Support/psst`;
- Linux configuration: `${XDG_CONFIG_HOME:-$HOME/.config}/psst`; state:
  `${XDG_DATA_HOME:-$HOME/.local/share}/psst`.

Durable launcher identity lives below the selected profile's `agents/` state directory. Claude's
strict MCP JSON is a temporary file in the platform runtime directory and is removed after normal
client exit. Claude and Codex start in a contained Psst directory rather than whichever repository
or vault happened to be open when the command was entered. Credentials remain in the separate
restricted credential store and never enter generated MCP JSON.

One profile represents one relay-bound squad membership. A separate launcher lock prevents two
agent harnesses from owning the same profile. Use a distinct profile for every team membership and
every simultaneously running agent.

## Claude Code

```text
psst --profile research-claude agent claude
```

Psst locates the installed `claude` command, writes a process-scoped strict MCP configuration, opts
that named server into the supported development Channel, and starts interactive Claude Code. It
never uses `claude -p`.

Resume Claude's own most recent compatible session with:

```text
psst --profile research-claude agent claude --continue
```

Permission skipping is never implicit. On a trusted disposable workspace, an operator may make the
separate explicit choice:

```text
psst --profile research-claude agent claude --continue --dangerously-skip-permissions
```

`PSST_CLAUDE_COMMAND` is a non-secret explicit executable override when Claude is not on `PATH`.

## Codex

```text
psst --profile research-codex agent codex
```

Psst locates Codex, validates the installed App Server contract, and creates exactly one durable
task record if the profile has never run. Later invocations resume that task automatically.

To require that a prior durable task already exists:

```text
psst --profile research-codex agent codex --continue
```

That command fails closed instead of creating a replacement task. `PSST_CODEX_COMMAND` is the
equivalent non-secret executable override when Codex is not on `PATH`.

## Status and shutdown

```text
psst --profile research-codex agent status
```

Status is passive and sanitized. Long-running launchers stay in the foreground and stop with
Ctrl+C or normal interactive-client exit. Retrieval remains distinct from acknowledgement, and the
relay inbox remains authoritative across launcher or network restarts.

Relay discovery, saved origins, and invitation-backed `team create|join` are the next Slice 7 work.
Until they land, the profile must already be joined and the ordinary CLI/environment/config relay
precedence still applies.
