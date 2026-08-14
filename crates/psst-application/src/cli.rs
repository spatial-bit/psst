use crate::{ExitClass, SafeError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const CLI_CONTRACT_VERSION: &str = "psst.cli.v1";
pub const DEFAULT_MAX_MESSAGE_BYTES: u64 = 65_536;
pub const MAX_MESSAGE_BYTES: u64 = 65_536;

/// Frozen Slice 3 command grammar. Behavior is assigned to later work units.
pub const CLI_HELP: &str = r"Psst — durable direct messages for cooperative AI agents

Usage: psst [GLOBAL OPTIONS] <command>

Global options:
  --relay <origin>       Relay origin (CLI > PSST_RELAY > config > default)
  --profile <name>       Local profile (CLI > PSST_PROFILE > config > default)
  --config <path>        Non-secret configuration file
  --json                 Emit one versioned JSON value on the designated stream
  -h, --help             Print help
  -V, --version          Print version

Commands:
  relay start [--bind <address>] [--data-dir <path>] [--allow-lan] [--log <level>]
  agent claude [--continue] [--dangerously-skip-permissions]
  agent codex [--continue]
  agent status
  health
  config show --effective
  profile list
  profile show [<name>]
  squad list
  squad create <squad> --mission <text>
  squad describe <squad>
  squad archive <squad>
  squad join <squad> --name <name> --role <role> [--mission <text>]
  squad leave
  squad roster
  message send --to <member> (--body <text> | --file <path|->) [--priority normal|high] [--reply-to <id>] [--correlation-id <id>]
  inbox [--limit <1..100>] [--wait <0..30>] [--ack <id>...]
  listen [--wait <1..30>] [--ack]
  message acknowledge <id>...
  transcript [--after <sequence>] [--limit <1..100>]
  status
  harness status
  database info
  database backup <destination>
  database integrity-check

In --json mode success writes exactly one JSON value plus a newline to stdout and writes nothing
to stderr. Failure writes exactly one JSON value plus a newline to stderr and writes nothing to
stdout. No surrounding prose is permitted. Sensitive authentication material never appears in
arguments, environment variables, output, help, diagnostics, configuration, or model-visible data.
";

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CliCommand {
    /// Failure-only identity for usage errors before an executable command is selected.
    Invocation,
    RelayStart,
    AgentClaude,
    AgentCodex,
    AgentStatus,
    Health,
    ConfigShowEffective,
    ProfileList,
    ProfileShow,
    SquadList,
    SquadCreate,
    SquadDescribe,
    SquadArchive,
    SquadJoin,
    SquadLeave,
    SquadRoster,
    MessageSend,
    Inbox,
    Listen,
    MessageAcknowledge,
    Transcript,
    Status,
    HarnessStatus,
    DatabaseInfo,
    DatabaseBackup,
    DatabaseIntegrityCheck,
}

#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CliSuccess<T> {
    #[schemars(extend("const" = "psst.cli.v1"))]
    version: String,
    #[schemars(extend("const" = true))]
    ok: bool,
    pub command: CliCommand,
    pub data: T,
}

impl<T> CliSuccess<T> {
    #[must_use]
    pub fn new(command: CliCommand, data: T) -> Self {
        Self {
            version: CLI_CONTRACT_VERSION.into(),
            ok: true,
            command,
            data,
        }
    }
}

#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CliFailure {
    #[schemars(extend("const" = "psst.cli.v1"))]
    version: String,
    #[schemars(extend("const" = false))]
    ok: bool,
    pub command: CliCommand,
    pub error: SafeError,
}

impl CliFailure {
    #[must_use]
    pub fn new(command: CliCommand, error: SafeError) -> Self {
        Self {
            version: CLI_CONTRACT_VERSION.into(),
            ok: false,
            command,
            error,
        }
    }
    #[must_use]
    pub const fn exit_class(&self) -> ExitClass {
        self.error.exit_class()
    }
}

/// Literal process emission contract used by the future CLI runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonEmission {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u8,
}

/// # Errors
/// Returns an error only if the typed payload cannot be represented as JSON.
pub fn emit_json_success<T: Serialize>(
    value: &CliSuccess<T>,
) -> Result<JsonEmission, serde_json::Error> {
    Ok(JsonEmission {
        stdout: serde_json::to_string(value)? + "\n",
        stderr: String::new(),
        exit_code: 0,
    })
}

/// # Errors
/// Returns an error only if the closed failure envelope cannot be represented as JSON.
pub fn emit_json_failure(value: &CliFailure) -> Result<JsonEmission, serde_json::Error> {
    Ok(JsonEmission {
        stdout: String::new(),
        stderr: serde_json::to_string(value)? + "\n",
        exit_code: value.exit_class().code(),
    })
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueSource {
    CommandLine,
    Environment,
    ConfigFile,
    Default,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveValue<T> {
    pub value: T,
    pub source: ValueSource,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialState {
    Absent,
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveConfigView {
    pub relay_origin: EffectiveValue<String>,
    pub profile: EffectiveValue<String>,
    pub config_path: EffectiveValue<String>,
    pub relay_bind: EffectiveValue<String>,
    pub relay_data_dir: EffectiveValue<String>,
    pub allow_lan: EffectiveValue<bool>,
    pub log_level: EffectiveValue<String>,
    pub log_format: EffectiveValue<String>,
    pub max_message_bytes: EffectiveValue<u64>,
    pub max_long_poll_seconds: EffectiveValue<u32>,
    pub heartbeat_interval_seconds: EffectiveValue<u32>,
    pub lease_seconds: EffectiveValue<u32>,
    pub credential_state: CredentialState,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileBindingView {
    pub profile: String,
    pub relay_origin: String,
    pub bound: bool,
    pub squad_id: Option<String>,
    pub member_id: Option<String>,
    pub credential_state: CredentialState,
}
