//! Human and operator command shell for Psst.

#![forbid(unsafe_code)]

use fs2::FileExt as _;
use psst_application::{
    CLI_HELP, CliCommand, CliFailure, CliSuccess, ConfigFlags, ConfigInputs, ConfigResolver,
    CredentialState, LocalErrorCode, PlatformPaths, ProfilePaths, ResolvedConfig, RuntimeSpec,
    SessionError, SessionHealth, SessionRuntime, UnboundRuntimeSpec, emit_json_failure,
    emit_json_success, harness_status_path, load_harness_status_view, load_profile,
    map_client_error, verify_profile_origin,
};
use psst_client::{Client, ClientConfig};
use psst_protocol::{
    AckMessagesRequest, AgentModeDto, ClientMetadata, CreateSquadRequest, InboxResponse,
    JoinSquadRequest, LeaveSquadResponse, MessagePriorityDto, MessageSequence,
};
use serde::Serialize;
use serde_json::Value;
use std::fmt::Write as _;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    future::Future,
    io::{self, Read, Write},
    net::SocketAddr,
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
    time::Duration,
};
use tokio::process::Command;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct GlobalOptions {
    relay: Option<String>,
    profile: Option<String>,
    config: Option<PathBuf>,
    json: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ParsedCommand {
    RelayStart(RelayStartArgs),
    AgentClaude(AgentClaudeArgs),
    AgentCodex(AgentCodexArgs),
    AgentStatus,
    InternalMcp,
    Health,
    ConfigShowEffective,
    HarnessStatus,
    SquadList,
    SquadCreate {
        squad: String,
        mission: String,
    },
    SquadDescribe {
        squad: String,
    },
    Deferred {
        command: CliCommand,
        arguments: Vec<OsString>,
    },
}

impl ParsedCommand {
    const fn contract(&self) -> CliCommand {
        match self {
            Self::RelayStart(_) => CliCommand::RelayStart,
            Self::AgentClaude(_) => CliCommand::AgentClaude,
            Self::AgentCodex(_) => CliCommand::AgentCodex,
            Self::AgentStatus => CliCommand::AgentStatus,
            Self::InternalMcp => CliCommand::Invocation,
            Self::Health => CliCommand::Health,
            Self::ConfigShowEffective => CliCommand::ConfigShowEffective,
            Self::HarnessStatus => CliCommand::HarnessStatus,
            Self::SquadList => CliCommand::SquadList,
            Self::SquadCreate { .. } => CliCommand::SquadCreate,
            Self::SquadDescribe { .. } => CliCommand::SquadDescribe,
            Self::Deferred { command, .. } => *command,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AgentClaudeArgs {
    continue_session: bool,
    dangerously_skip_permissions: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AgentCodexArgs {
    continue_session: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RelayStartArgs {
    bind: Option<SocketAddr>,
    data_dir: Option<PathBuf>,
    allow_lan: bool,
    log_level: Option<String>,
}

#[derive(Debug)]
enum Invocation {
    Help,
    Version,
    Command(GlobalOptions, ParsedCommand),
}

#[derive(Debug)]
struct ParseError {
    command: Option<CliCommand>,
    json: bool,
}

/// Runs the process command line and writes only through the frozen stdout/stderr boundary.
pub async fn run_process(arguments: impl IntoIterator<Item = OsString>) -> ExitCode {
    // Do not hold process-wide output locks across asynchronous command execution. Relay request
    // tracing can run on another Tokio worker; a long-lived stderr lock here would block that
    // worker before Hyper can publish the completed response.
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match parse(arguments.clone()) {
        Ok(Invocation::Command(options, ParsedCommand::InternalMcp)) => {
            return ExitCode::from(run_internal_mcp(&options).await);
        }
        Ok(Invocation::Command(options, ParsedCommand::AgentClaude(arguments))) => {
            return ExitCode::from(run_agent_claude(&options, arguments).await);
        }
        Ok(Invocation::Command(options, ParsedCommand::AgentCodex(arguments))) => {
            return ExitCode::from(run_agent_codex(&options, arguments).await);
        }
        _ => {}
    }
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    let code = run_with_io(arguments, &mut stdout, &mut stderr).await;
    ExitCode::from(code)
}

async fn run_internal_mcp(options: &GlobalOptions) -> u8 {
    if options.json
        || options.config.is_some()
        || options.profile.is_some()
        || options.relay.is_some()
    {
        eprintln!("psst: invalid internal MCP invocation");
        return 64;
    }
    match psst_mcp::serve_configured_stdio().await {
        Ok(()) => 0,
        Err(psst_mcp::ConfiguredStdioError::Startup) => {
            eprintln!("psst: cooperative MCP startup failed");
            70
        }
        Err(psst_mcp::ConfiguredStdioError::Protocol) => {
            eprintln!("psst: MCP protocol session failed");
            70
        }
    }
}

async fn run_agent_codex(options: &GlobalOptions, arguments: AgentCodexArgs) -> u8 {
    if options.json {
        eprintln!("psst: agent processes require an interactive terminal");
        return 64;
    }
    let Ok(resolved) = resolve_config(options, None) else {
        eprintln!("psst: Codex agent configuration is invalid");
        return 78;
    };
    if !bound_agent_profile(&resolved) {
        eprintln!("psst: Codex agent profile is not bound to this relay");
        return 78;
    }
    let Ok(executable) = std::env::current_exe() else {
        eprintln!("psst: agent launcher unavailable");
        return 70;
    };
    let directory = resolved
        .paths
        .data_dir
        .join("agents")
        .join(&resolved.profile.value);
    if std::fs::create_dir_all(&directory).is_err() {
        eprintln!("psst: Codex agent state directory could not be created");
        return 74;
    }
    let Ok(_launcher) = AgentLaunchGuard::acquire(&directory) else {
        eprintln!("psst: this agent profile is already running");
        return 75;
    };
    let record = directory.join("codex-thread-id");
    let thread = if let Ok(Some(identifier)) = load_codex_thread_record(&record) {
        psst_codex::ThreadPolicy::Resume(identifier)
    } else if record.exists() {
        eprintln!("psst: Codex task record is invalid");
        return 78;
    } else if arguments.continue_session {
        eprintln!("psst: no saved Codex task exists for this profile");
        return 78;
    } else {
        psst_codex::ThreadPolicy::Create {
            record: record.clone(),
        }
    };
    let Some((codex_command, codex_args)) = resolve_local_command("codex", "PSST_CODEX_COMMAND")
    else {
        eprintln!("psst: Codex could not be located; install it or set PSST_CODEX_COMMAND");
        return 69;
    };
    let mut mcp_environment = BTreeMap::from([
        ("PSST_PROFILE".to_owned(), resolved.profile.value.clone()),
        ("PSST_RELAY".to_owned(), resolved.relay_origin.value.clone()),
    ]);
    for key in [
        "APPDATA",
        "LOCALAPPDATA",
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_RUNTIME_DIR",
    ] {
        if let Ok(value) = std::env::var(key)
            && !value.is_empty()
        {
            mcp_environment.insert(key.to_owned(), value);
        }
    }
    let app_server = psst_codex::AppServerConfig {
        command: codex_command,
        command_args: codex_args,
        mcp_command: executable,
        mcp_args: vec!["internal".into(), "mcp".into()],
        mcp_environment,
        thread,
        cwd: directory,
    };
    let activation = psst_codex::start_with_app_server(resolved, app_server).await;
    let Ok(activation) = activation else {
        eprintln!("psst: Codex agent startup failed");
        return 70;
    };
    eprintln!("Psst Codex agent is running; press Ctrl+C to stop.");
    shutdown_signal().await;
    if activation.shutdown().await.is_err() {
        eprintln!("psst: Codex agent shutdown failed");
        return 70;
    }
    0
}

async fn run_agent_claude(options: &GlobalOptions, arguments: AgentClaudeArgs) -> u8 {
    if options.json {
        eprintln!("psst: agent processes require an interactive terminal");
        return 64;
    }
    let Ok(resolved) = resolve_config(options, None) else {
        eprintln!("psst: Claude agent configuration is invalid");
        return 78;
    };
    if !bound_agent_profile(&resolved) {
        eprintln!("psst: Claude agent profile is not bound to this relay");
        return 78;
    }
    let Ok(executable) = std::env::current_exe() else {
        eprintln!("psst: agent launcher unavailable");
        return 70;
    };
    let server_name = format!("psst-{}", resolved.profile.value);
    if !valid_local_server_name(&server_name) {
        eprintln!("psst: profile cannot be used as a Claude server name");
        return 78;
    }
    let directory = resolved
        .paths
        .runtime_dir
        .join("agents")
        .join(&resolved.profile.value);
    if std::fs::create_dir_all(&directory).is_err() {
        eprintln!("psst: Claude agent configuration could not be created");
        return 74;
    }
    let lock_directory = resolved
        .paths
        .data_dir
        .join("agents")
        .join(&resolved.profile.value);
    if std::fs::create_dir_all(&lock_directory).is_err() {
        eprintln!("psst: Claude agent state directory could not be created");
        return 74;
    }
    let Ok(_launcher) = AgentLaunchGuard::acquire(&lock_directory) else {
        eprintln!("psst: this agent profile is already running");
        return 75;
    };
    let Ok(config) = write_claude_mcp_config(&resolved, &executable, &server_name, &directory)
    else {
        eprintln!("psst: Claude agent configuration could not be written");
        return 74;
    };
    let Some((claude_command, claude_args)) =
        resolve_local_command("claude", "PSST_CLAUDE_COMMAND")
    else {
        eprintln!("psst: Claude could not be located; install it or set PSST_CLAUDE_COMMAND");
        return 69;
    };
    let mut command = Command::new(claude_command);
    command
        .args(claude_args)
        .arg("--strict-mcp-config")
        .arg("--mcp-config")
        .arg(config.path())
        .arg("--dangerously-load-development-channels")
        .arg(format!("server:{server_name}"))
        .current_dir(&directory);
    if arguments.continue_session {
        command.arg("--continue");
    }
    if arguments.dangerously_skip_permissions {
        command.arg("--dangerously-skip-permissions");
    }
    let status = command.status().await;
    match status {
        Ok(status) if status.success() => 0,
        Ok(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(70),
        Err(_) => {
            eprintln!("psst: Claude could not be launched; verify that `claude` is installed");
            69
        }
    }
}

fn valid_local_server_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn write_claude_mcp_config(
    resolved: &ResolvedConfig,
    executable: &std::path::Path,
    server_name: &str,
    directory: &std::path::Path,
) -> Result<tempfile::NamedTempFile, ()> {
    let config = serde_json::json!({
        "mcpServers": {
            server_name: {
                "type": "stdio",
                "command": executable,
                "args": ["internal", "mcp"],
                "env": {
                    "PSST_RELAY": resolved.relay_origin.value,
                    "PSST_PROFILE": resolved.profile.value,
                    "PSST_CLAUDE_CHANNEL": "enabled"
                }
            }
        }
    });
    let encoded = serde_json::to_vec_pretty(&config).map_err(|_| ())?;
    let mut file = tempfile::Builder::new()
        .prefix("claude-mcp-")
        .suffix(".json")
        .tempfile_in(directory)
        .map_err(|_| ())?;
    file.write_all(&encoded).map_err(|_| ())?;
    file.flush().map_err(|_| ())?;
    Ok(file)
}

fn valid_agent_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn load_codex_thread_record(path: &std::path::Path) -> Result<Option<String>, ()> {
    if !path.exists() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(path).map_err(|_| ())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 129 {
        return Err(());
    }
    let identifier = std::fs::read_to_string(path).map_err(|_| ())?;
    let identifier = identifier.trim().to_owned();
    if !valid_agent_identifier(&identifier) {
        return Err(());
    }
    Ok(Some(identifier))
}

fn bound_agent_profile(resolved: &ResolvedConfig) -> bool {
    let Ok(paths) = ProfilePaths::for_profile(
        &resolved.paths,
        &resolved.relay_origin.value,
        &resolved.profile.value,
    ) else {
        return false;
    };
    let Ok(Some(binding)) = load_profile(&paths.metadata) else {
        return false;
    };
    verify_profile_origin(&binding, &resolved.relay_origin.value).is_ok()
}

fn resolve_local_command(name: &str, override_name: &str) -> Option<(PathBuf, Vec<String>)> {
    if let Some(path) = std::env::var_os(override_name).map(PathBuf::from) {
        return executable_command(path);
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        #[cfg(windows)]
        let candidates = [
            directory.join(format!("{name}.exe")),
            directory.join(format!("{name}.cmd")),
            directory.join(format!("{name}.bat")),
        ];
        #[cfg(not(windows))]
        let candidates = [directory.join(name)];
        for candidate in candidates {
            if let Some(command) = executable_command(candidate) {
                return Some(command);
            }
        }
    }
    None
}

fn executable_command(path: PathBuf) -> Option<(PathBuf, Vec<String>)> {
    if !path.is_file() {
        return None;
    }
    #[cfg(windows)]
    if path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
    {
        let shell = std::env::var_os("ComSpec").map(PathBuf::from)?;
        if !shell.is_file() {
            return None;
        }
        return Some((
            shell,
            vec![
                "/d".into(),
                "/c".into(),
                path.to_string_lossy().into_owned(),
            ],
        ));
    }
    Some((path, Vec::new()))
}

struct AgentLaunchGuard(std::fs::File);

impl AgentLaunchGuard {
    fn acquire(directory: &std::path::Path) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("launcher.lock"))?;
        file.try_lock_exclusive()?;
        Ok(Self(file))
    }
}

impl Drop for AgentLaunchGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

async fn run_with_io(
    arguments: impl IntoIterator<Item = OsString>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> u8 {
    match parse(arguments) {
        Ok(Invocation::Help) => write_bytes(stdout, CLI_HELP.as_bytes(), 0),
        Ok(Invocation::Version) => write_bytes(stdout, format!("psst {VERSION}\n").as_bytes(), 0),
        Ok(Invocation::Command(options, ParsedCommand::RelayStart(arguments))) if options.json => {
            run_relay_json_until(&options, &arguments, shutdown_signal(), stdout, stderr).await
        }
        Ok(Invocation::Command(options, command)) => {
            let contract = command.contract();
            match execute(&options, command).await {
                Ok(result) => emit_success(options.json, contract, result, stdout, stderr),
                Err(error) => emit_failure(options.json, contract, error, stdout, stderr),
            }
        }
        Err(error) => {
            if error.json {
                emit_failure(
                    true,
                    error.command.unwrap_or(CliCommand::Invocation),
                    LocalErrorCode::InvalidInput,
                    stdout,
                    stderr,
                )
            } else {
                write_bytes(stderr, b"psst: invalid command line\n", 2)
            }
        }
    }
}

fn write_bytes(writer: &mut impl Write, bytes: &[u8], code: u8) -> u8 {
    if writer.write_all(bytes).is_ok() && writer.flush().is_ok() {
        code
    } else {
        70
    }
}

fn emit_success<T: Serialize>(
    json: bool,
    command: CliCommand,
    data: T,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> u8 {
    if json {
        match emit_json_success(&CliSuccess::new(command, data)) {
            Ok(emission) => write_bytes(stdout, emission.stdout.as_bytes(), emission.exit_code),
            Err(_) => write_bytes(stderr, b"psst: output encoding failed\n", 70),
        }
    } else {
        match serde_json::to_value(data) {
            Ok(value) => write_human(command, &value, stdout),
            Err(_) => write_bytes(stderr, b"psst: output encoding failed\n", 70),
        }
    }
}

fn emit_failure(
    json: bool,
    command: CliCommand,
    code: LocalErrorCode,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> u8 {
    let failure = CliFailure::new(command, code.into());
    if json {
        match emit_json_failure(&failure) {
            Ok(emission) => write_bytes(stderr, emission.stderr.as_bytes(), emission.exit_code),
            Err(_) => write_bytes(stderr, b"psst: output encoding failed\n", 70),
        }
    } else {
        let line = format!("psst: {}\n", code.safe_message());
        let _ = stdout;
        write_bytes(stderr, line.as_bytes(), failure.exit_class().code())
    }
}

fn write_human(command: CliCommand, value: &Value, stdout: &mut impl Write) -> u8 {
    let rendered = match command {
        CliCommand::Health => format!(
            "relay: {}\ndatabase: {} (schema {})\n",
            value["health"]["status"].as_str().unwrap_or("unknown"),
            value["ready"]["status"].as_str().unwrap_or("unknown"),
            value["ready"]["schema_version"].as_u64().unwrap_or(0)
        ),
        CliCommand::SquadList => {
            let mut output = String::new();
            if let Some(squads) = value.as_array() {
                for squad in squads {
                    let _ = writeln!(
                        output,
                        "{}\t{}\t{}",
                        squad["name"].as_str().unwrap_or("?"),
                        squad["state"].as_str().unwrap_or("?"),
                        squad["mission"].as_str().unwrap_or("")
                    );
                }
            }
            output
        }
        CliCommand::SquadCreate | CliCommand::SquadDescribe => format!(
            "{} [{}]\nmission: {}\n",
            value["name"].as_str().unwrap_or("?"),
            value["state"].as_str().unwrap_or("?"),
            value["mission"].as_str().unwrap_or("")
        ),
        CliCommand::ConfigShowEffective => serde_json::to_string_pretty(value).map_or_else(
            |_| "psst: output encoding failed\n".into(),
            |text| text + "\n",
        ),
        _ => serde_json::to_string_pretty(value).map_or_else(
            |_| "psst: output encoding failed\n".into(),
            |text| text + "\n",
        ),
    };
    write_bytes(stdout, rendered.as_bytes(), 0)
}

async fn execute(options: &GlobalOptions, command: ParsedCommand) -> Result<Value, LocalErrorCode> {
    match command {
        ParsedCommand::RelayStart(arguments) => {
            let resolved = resolve_config(options, Some(&arguments))?;
            run_relay(options, &resolved).await
        }
        ParsedCommand::Health => {
            let client = client(&resolve_config(options, None)?)?;
            let health = client
                .health()
                .await
                .map_err(|error| map_client_error(&error))?;
            let ready = client
                .ready()
                .await
                .map_err(|error| map_client_error(&error))?;
            Ok(serde_json::json!({"health":health,"ready":ready}))
        }
        ParsedCommand::ConfigShowEffective => {
            let resolved = resolve_config(options, None)?;
            serde_json::to_value(resolved.view(credential_state(&resolved)))
                .map_err(|_| LocalErrorCode::Internal)
        }
        ParsedCommand::HarnessStatus | ParsedCommand::AgentStatus => {
            let resolved = resolve_config(options, None)?;
            execute_harness_status(&resolved)
        }
        ParsedCommand::AgentClaude(_)
        | ParsedCommand::AgentCodex(_)
        | ParsedCommand::InternalMcp => Err(LocalErrorCode::Internal),
        ParsedCommand::SquadList => serde_json::to_value(
            client(&resolve_config(options, None)?)?
                .list_squads()
                .await
                .map_err(|error| map_client_error(&error))?,
        )
        .map_err(|_| LocalErrorCode::Internal),
        ParsedCommand::SquadCreate { squad, mission } => serde_json::to_value(
            client(&resolve_config(options, None)?)?
                .create_squad(&CreateSquadRequest {
                    name: squad,
                    mission,
                })
                .await
                .map_err(|error| map_client_error(&error))?,
        )
        .map_err(|_| LocalErrorCode::Internal),
        ParsedCommand::SquadDescribe { squad } => serde_json::to_value(
            client(&resolve_config(options, None)?)?
                .describe_squad(&squad)
                .await
                .map_err(|error| map_client_error(&error))?,
        )
        .map_err(|_| LocalErrorCode::Internal),
        ParsedCommand::Deferred { command, arguments } => {
            execute_protected(options, command, &arguments).await
        }
    }
}

fn execute_harness_status(resolved: &ResolvedConfig) -> Result<Value, LocalErrorCode> {
    let paths = psst_application::ProfilePaths::for_profile(
        &resolved.paths,
        &resolved.relay_origin.value,
        &resolved.profile.value,
    )
    .map_err(|_| LocalErrorCode::InvalidConfiguration)?;
    let path = harness_status_path(&paths).map_err(|_| LocalErrorCode::InvalidConfiguration)?;
    let record = load_harness_status_view(&path).map_err(|error| input_io(&error))?;
    to_value(record.ok_or(LocalErrorCode::InvalidSession)?)
}

struct ProfileSession {
    client: Arc<Client>,
    runtime: SessionRuntime,
}

fn local_io(error: &io::Error) -> LocalErrorCode {
    match error.kind() {
        io::ErrorKind::PermissionDenied => LocalErrorCode::LocalPermission,
        io::ErrorKind::WouldBlock | io::ErrorKind::AddrInUse => LocalErrorCode::ProfileLocked,
        io::ErrorKind::AlreadyExists => LocalErrorCode::ProfileAlreadyBound,
        io::ErrorKind::NotFound => LocalErrorCode::InvalidSession,
        _ => LocalErrorCode::LocalRead,
    }
}

fn input_io(error: &io::Error) -> LocalErrorCode {
    if error.kind() == io::ErrorKind::PermissionDenied {
        LocalErrorCode::LocalPermission
    } else {
        LocalErrorCode::LocalRead
    }
}

fn map_session_error(error: &SessionError) -> LocalErrorCode {
    match error {
        SessionError::Local(error) => local_io(error),
        SessionError::Relay(error) => map_client_error(error),
        SessionError::ShutdownTimedOut | SessionError::RecoveryOutcomeUnknown => {
            LocalErrorCode::OutcomeUnknown
        }
        SessionError::NotReady => LocalErrorCode::InvalidSession,
        SessionError::Unbound => LocalErrorCode::ProfileUnbound,
        SessionError::SendCapacity | SessionError::OperationCapacity => LocalErrorCode::LocalLock,
    }
}

async fn open_session(resolved: &ResolvedConfig) -> Result<ProfileSession, LocalErrorCode> {
    let paths = psst_application::ProfilePaths::for_profile(
        &resolved.paths,
        &resolved.relay_origin.value,
        &resolved.profile.value,
    )
    .map_err(|_| LocalErrorCode::InvalidConfiguration)?;
    let mut binding = load_profile(&paths.metadata).map_err(|error| local_io(&error))?;
    if binding.is_none() {
        SessionRuntime::recover_orphaned_leave(
            paths.clone(),
            resolved.relay_origin.value.clone(),
            resolved.profile.value.clone(),
        )
        .await
        .map_err(|error| map_session_error(&error))?;
        binding = load_profile(&paths.metadata).map_err(|error| local_io(&error))?;
    }
    let binding = binding.ok_or(LocalErrorCode::ProfileUnbound)?;
    psst_application::verify_profile_origin(&binding, &resolved.relay_origin.value)
        .map_err(|_| LocalErrorCode::ProfileOriginMismatch)?;
    let client = Arc::new(client(resolved)?);
    let runtime = SessionRuntime::start(
        client.clone(),
        RuntimeSpec {
            profile: binding,
            paths,
            mode: AgentModeDto::Cooperative,
            client_metadata: cli_metadata(),
            shutdown_bound: Duration::from_secs(5),
        },
    )
    .await
    .map_err(|error| map_session_error(&error))?;
    Ok(ProfileSession { client, runtime })
}

fn cli_metadata() -> ClientMetadata {
    ClientMetadata {
        kind: "psst-cli".into(),
        hostname: None,
        version: Some(VERSION.into()),
    }
}

async fn execute_protected(
    options: &GlobalOptions,
    command: CliCommand,
    args: &[OsString],
) -> Result<Value, LocalErrorCode> {
    let resolved = resolve_config(options, None)?;
    if command == CliCommand::SquadJoin {
        return execute_join(&resolved, args).await;
    }
    let session = open_session(&resolved).await?;
    let result = match command {
        CliCommand::SquadLeave => {
            let leave = session.runtime.leave().await;
            return finish_leave(leave, session.runtime.shutdown()).await;
        }
        CliCommand::SquadArchive => {
            let authority = session
                .runtime
                .authority()
                .await
                .map_err(|error| map_session_error(&error))?;
            if nonempty(args.get(2)).as_deref() == Some(authority.squad_name.as_str()) {
                to_value(
                    session
                        .runtime
                        .archive()
                        .await
                        .map_err(|error| map_session_error(&error))?,
                )
            } else {
                Err(LocalErrorCode::AuthorityDenied)
            }
        }
        CliCommand::SquadRoster => to_value(
            session
                .runtime
                .roster()
                .await
                .map_err(|error| map_session_error(&error))?,
        ),
        CliCommand::Status => {
            let snapshot = session.runtime.snapshot().await;
            Ok(serde_json::json!({
                "health": session_health_name(snapshot.health),
                "generation": snapshot.generation,
                "instance_id": snapshot.instance_id,
                "heartbeat_interval_seconds": snapshot.heartbeat_interval_seconds,
                "lease_expires_at": snapshot.lease_expires_at,
            }))
        }
        CliCommand::MessageSend => execute_send(&resolved, &session, args).await,
        CliCommand::Inbox => execute_inbox(&session, args).await,
        CliCommand::MessageAcknowledge => execute_ack(&session, &args[2..]).await,
        CliCommand::Transcript => execute_transcript(&session, args).await,
        CliCommand::Listen => execute_listen_until(&session, args, shutdown_signal()).await,
        _ => Err(LocalErrorCode::Unsupported),
    };
    let shutdown = session.runtime.shutdown().await;
    match (result, shutdown) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(map_session_error(&error)),
        (Ok(value), Ok(())) => Ok(value),
    }
}

async fn finish_leave(
    leave: Result<LeaveSquadResponse, SessionError>,
    shutdown: impl Future<Output = Result<(), SessionError>>,
) -> Result<Value, LocalErrorCode> {
    match leave {
        Ok(response) => to_value(response),
        Err(error) => {
            // The leave failure is the command outcome. Shutdown is still awaited to reap the
            // runtime and release its profile lock, but a shutdown failure must not mask the more
            // specific leave error.
            let _ = shutdown.await;
            Err(map_session_error(&error))
        }
    }
}

const fn session_health_name(health: SessionHealth) -> &'static str {
    match health {
        SessionHealth::Ready => "ready",
        SessionHealth::Degraded => "degraded",
        SessionHealth::OutcomeUnknown => "outcome_unknown",
        SessionHealth::RotationFailed => "rotation_failed",
        SessionHealth::Stopped => "stopped",
    }
}

fn to_value(value: impl Serialize) -> Result<Value, LocalErrorCode> {
    serde_json::to_value(value).map_err(|_| LocalErrorCode::Internal)
}

async fn execute_join(
    resolved: &ResolvedConfig,
    args: &[OsString],
) -> Result<Value, LocalErrorCode> {
    let values = parse_option_pairs(&args[3..], &["--name", "--role", "--mission"])
        .ok_or(LocalErrorCode::InvalidInput)?;
    let squad = nonempty(args.get(2)).ok_or(LocalErrorCode::InvalidInput)?;
    let paths = psst_application::ProfilePaths::for_profile(
        &resolved.paths,
        &resolved.relay_origin.value,
        &resolved.profile.value,
    )
    .map_err(|_| LocalErrorCode::InvalidConfiguration)?;
    let client = Arc::new(client(resolved)?);
    let joined = SessionRuntime::join_and_bind(
        client,
        UnboundRuntimeSpec {
            relay_origin: resolved.relay_origin.value.clone(),
            profile_name: resolved.profile.value.clone(),
            squad,
            paths,
            shutdown_bound: Duration::from_secs(5),
        },
        JoinSquadRequest {
            name: values["--name"].clone(),
            role: values["--role"].clone(),
            mode: AgentModeDto::Cooperative,
            client: cli_metadata(),
            mission: values.get("--mission").cloned(),
        },
    )
    .await
    .map_err(|error| map_session_error(&error))?;
    let response = to_value(joined.response)?;
    joined
        .runtime
        .shutdown()
        .await
        .map_err(|error| map_session_error(&error))?;
    Ok(response)
}

async fn execute_send(
    resolved: &ResolvedConfig,
    session: &ProfileSession,
    args: &[OsString],
) -> Result<Value, LocalErrorCode> {
    let values = parse_option_pairs(
        &args[2..],
        &[
            "--to",
            "--body",
            "--file",
            "--priority",
            "--reply-to",
            "--correlation-id",
        ],
    )
    .expect("validated grammar");
    let body = if let Some(body) = values.get("--body") {
        if body.len() as u64 > resolved.max_message_bytes.value {
            return Err(LocalErrorCode::PayloadTooLarge);
        }
        body.clone()
    } else {
        let path = &values["--file"];
        if path == "-" {
            read_bounded_utf8(io::stdin(), resolved.max_message_bytes.value)?
        } else {
            let metadata = std::fs::metadata(path).map_err(|error| input_io(&error))?;
            if metadata.len() > resolved.max_message_bytes.value {
                return Err(LocalErrorCode::PayloadTooLarge);
            }
            let file = std::fs::File::open(path).map_err(|error| input_io(&error))?;
            read_bounded_utf8(file, resolved.max_message_bytes.value)?
        }
    };
    let priority = if values.get("--priority").is_some_and(|v| v == "high") {
        MessagePriorityDto::High
    } else {
        MessagePriorityDto::Normal
    };
    let prepared = session
        .client
        .prepare_send(
            values["--to"].clone(),
            body,
            priority,
            values.get("--reply-to").cloned(),
            values.get("--correlation-id").cloned(),
        )
        .map_err(|error| map_client_error(&error))?;
    to_value(
        session
            .runtime
            .send_prepared(&prepared)
            .await
            .map_err(|error| map_session_error(&error))?,
    )
}

fn read_bounded_utf8(reader: impl Read, maximum: u64) -> Result<String, LocalErrorCode> {
    let mut bytes = Vec::new();
    reader
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| LocalErrorCode::LocalRead)?;
    if bytes.len() as u64 > maximum {
        return Err(LocalErrorCode::PayloadTooLarge);
    }
    String::from_utf8(bytes).map_err(|_| LocalErrorCode::InvalidInput)
}

async fn execute_inbox(
    session: &ProfileSession,
    args: &[OsString],
) -> Result<Value, LocalErrorCode> {
    let (limit, wait, ack) = inbox_arguments(args);
    if !ack.is_empty() {
        session
            .runtime
            .acknowledge(&AckMessagesRequest { message_ids: ack })
            .await
            .map_err(|error| map_session_error(&error))?;
    }
    to_value(
        session
            .runtime
            .inbox(limit, wait)
            .await
            .map_err(|error| map_session_error(&error))?,
    )
}

async fn execute_ack(session: &ProfileSession, ids: &[OsString]) -> Result<Value, LocalErrorCode> {
    let request = AckMessagesRequest {
        message_ids: ids
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect(),
    };
    to_value(
        session
            .runtime
            .acknowledge(&request)
            .await
            .map_err(|error| map_session_error(&error))?,
    )
}

async fn execute_transcript(
    session: &ProfileSession,
    args: &[OsString],
) -> Result<Value, LocalErrorCode> {
    let values =
        parse_option_pairs(&args[1..], &["--after", "--limit"]).expect("validated grammar");
    let after =
        MessageSequence::new(values.get("--after").map_or(0, |v| v.parse().unwrap())).unwrap();
    let limit = values.get("--limit").map_or(100, |v| v.parse().unwrap());
    to_value(
        session
            .runtime
            .transcript(after, limit)
            .await
            .map_err(|error| map_session_error(&error))?,
    )
}

async fn execute_listen_until(
    session: &ProfileSession,
    args: &[OsString],
    cancellation: impl Future<Output = ()>,
) -> Result<Value, LocalErrorCode> {
    let wait = args
        .windows(2)
        .find(|v| v[0] == "--wait")
        .map_or(30, |v| v[1].to_string_lossy().parse().unwrap());
    tokio::pin!(cancellation);
    let response = loop {
        let poll = session.runtime.inbox(100, wait);
        let (response, cancelled) = tokio::select! {
            () = &mut cancellation => (InboxResponse {
                messages: Vec::new(),
                pending_count: 0,
                highest_priority: None,
                oldest_message_id: None,
            }, true),
            response = poll => (response.map_err(|error| map_session_error(&error))?, false),
        };
        if !response.messages.is_empty() || wait == 0 || cancelled {
            break response;
        }
    };
    if args.iter().any(|v| v == "--ack") && !response.messages.is_empty() {
        session
            .runtime
            .acknowledge(&AckMessagesRequest {
                message_ids: response.messages.iter().map(|m| m.id.clone()).collect(),
            })
            .await
            .map_err(|error| map_session_error(&error))?;
    }
    to_value(response)
}

fn inbox_arguments(args: &[OsString]) -> (u16, u8, Vec<String>) {
    let mut limit = 100;
    let mut wait = 0;
    let mut ack = Vec::new();
    let mut index = 1;
    while index < args.len() {
        match args[index].to_str() {
            Some("--limit") => {
                index += 1;
                limit = args[index].to_string_lossy().parse().unwrap();
            }
            Some("--wait") => {
                index += 1;
                wait = args[index].to_string_lossy().parse().unwrap();
            }
            Some("--ack") => {
                index += 1;
                while index < args.len() && !args[index].to_string_lossy().starts_with("--") {
                    ack.push(args[index].to_string_lossy().into_owned());
                    index += 1;
                }
                continue;
            }
            _ => unreachable!(),
        }
        index += 1;
    }
    (limit, wait, ack)
}

fn client(config: &ResolvedConfig) -> Result<Client, LocalErrorCode> {
    Client::new(&config.relay_origin.value, ClientConfig::default())
        .map_err(|error| map_client_error(&error))
}

fn resolve_config(
    options: &GlobalOptions,
    relay: Option<&RelayStartArgs>,
) -> Result<ResolvedConfig, LocalErrorCode> {
    let paths = PlatformPaths::detect().map_err(|_| LocalErrorCode::InvalidConfiguration)?;
    let relay = relay.cloned().unwrap_or_default();
    let flags = ConfigFlags {
        relay_origin: options.relay.clone(),
        profile: options.profile.clone(),
        config_path: options.config.clone(),
        relay_bind: relay.bind.map(|value| value.to_string()),
        data_dir: relay.data_dir,
        allow_lan: relay.allow_lan.then(|| "true".into()),
        log_level: relay.log_level,
        ..ConfigFlags::default()
    };
    let environment = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect::<BTreeMap<_, _>>();
    ConfigResolver::new(paths)
        .resolve(&ConfigInputs { flags, environment })
        .map_err(|_| LocalErrorCode::InvalidConfiguration)
}

fn credential_state(config: &ResolvedConfig) -> CredentialState {
    let Ok(paths) = psst_application::ProfilePaths::for_profile(
        &config.paths,
        &config.relay_origin.value,
        &config.profile.value,
    ) else {
        return CredentialState::Unavailable;
    };
    match paths.credential.try_exists() {
        Ok(true) => CredentialState::Available,
        Ok(false) => CredentialState::Absent,
        Err(_) => CredentialState::Unavailable,
    }
}

async fn run_relay(
    options: &GlobalOptions,
    resolved: &ResolvedConfig,
) -> Result<Value, LocalErrorCode> {
    run_relay_until(options.json, true, resolved, shutdown_signal()).await
}

#[cfg(windows)]
async fn shutdown_signal() {
    let Ok(mut ctrl_break) = tokio::signal::windows::ctrl_break() else {
        wait_for_successful_signal(tokio::signal::ctrl_c()).await;
        return;
    };
    // Interactive Ctrl-C is supported. The child-process test uses targeted Ctrl-Break because
    // Windows cannot safely target CTRL_C_EVENT at one process without broadcasting to its console.
    let _ = first_shutdown_signal(
        wait_for_successful_signal(tokio::signal::ctrl_c()),
        wait_for_present_signal(ctrl_break.recv()),
    )
    .await;
}

#[cfg(not(windows))]
async fn shutdown_signal() {
    // Tokio's Ctrl-C future is backed by SIGINT on Unix.
    wait_for_successful_signal(tokio::signal::ctrl_c()).await;
}

async fn wait_for_successful_signal<T, E>(signal: impl Future<Output = Result<T, E>>) {
    if signal.await.is_err() {
        std::future::pending::<()>().await;
    }
}

#[cfg(any(windows, test))]
async fn wait_for_present_signal<T>(signal: impl Future<Output = Option<T>>) {
    if signal.await.is_none() {
        std::future::pending::<()>().await;
    }
}

#[cfg(any(windows, test))]
async fn first_shutdown_signal<P, A>(
    primary: impl Future<Output = P>,
    alternate: impl Future<Output = A>,
) -> bool {
    tokio::pin!(primary);
    tokio::pin!(alternate);
    tokio::select! {
        _ = &mut primary => true,
        _ = &mut alternate => false,
    }
}

async fn run_relay_until(
    json: bool,
    hard_exit_on_timeout: bool,
    resolved: &ResolvedConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<Value, LocalErrorCode> {
    let (data_dir, config) = relay_config(resolved)?;
    if !json {
        psst_relay::init_tracing(config.log_format, &config.log_level)
            .map_err(|_| LocalErrorCode::InvalidConfiguration)?;
    }
    std::fs::create_dir_all(&data_dir).map_err(|error| map_io_error(&error))?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown.await;
        let _ = shutdown_tx.send(true);
    });
    if let Err(error) = psst_relay::serve(config, shutdown_rx).await {
        handle_relay_error(error.as_ref(), hard_exit_on_timeout)?;
    }
    Ok(serde_json::json!({"shutdown": {"clean": true}}))
}

fn relay_config(
    resolved: &ResolvedConfig,
) -> Result<(PathBuf, psst_relay::RelayConfig), LocalErrorCode> {
    let data_dir = PathBuf::from(&resolved.relay_data_dir.value);
    let mut config = psst_relay::RelayConfig::local(data_dir.join("psst.db"));
    config.bind = resolved
        .relay_bind
        .value
        .parse()
        .map_err(|_| LocalErrorCode::InvalidConfiguration)?;
    config.allow_lan = resolved.allow_lan.value;
    config.log_level.clone_from(&resolved.log_level.value);
    config.log_format = match resolved.log_format.value.as_str() {
        "text" => psst_relay::LogFormat::Text,
        "json" => psst_relay::LogFormat::Json,
        _ => return Err(LocalErrorCode::InvalidConfiguration),
    };
    config
        .validate()
        .map_err(|_| LocalErrorCode::InvalidConfiguration)?;
    Ok((data_dir, config))
}

async fn run_relay_json_until(
    options: &GlobalOptions,
    arguments: &RelayStartArgs,
    shutdown: impl Future<Output = ()> + Send + 'static,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> u8 {
    let resolved = match resolve_config(options, Some(arguments)) {
        Ok(resolved) => resolved,
        Err(error) => {
            return emit_failure(true, CliCommand::RelayStart, error, stdout, stderr);
        }
    };
    let (data_dir, config) = match relay_config(&resolved) {
        Ok(config) => config,
        Err(error) => {
            return emit_failure(true, CliCommand::RelayStart, error, stdout, stderr);
        }
    };
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        return emit_failure(
            true,
            CliCommand::RelayStart,
            map_io_error(&error),
            stdout,
            stderr,
        );
    }
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown.await;
        let _ = shutdown_tx.send(true);
    });
    let (startup_tx, startup_rx) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(psst_relay::serve_with_startup(
        config,
        shutdown_rx,
        startup_tx,
    ));
    let Ok(startup) = startup_rx.await else {
        let error = match serving.await {
            Ok(Err(error)) => map_relay_serve_error(error.as_ref()),
            _ => LocalErrorCode::Internal,
        };
        return emit_failure(true, CliCommand::RelayStart, error, stdout, stderr);
    };
    let data = serde_json::json!({
        "running": true,
        "bind": startup.bind.to_string(),
        "database": startup.database.display().to_string(),
        "schema_version": startup.schema_version,
        "trusted_lan": startup.trusted_lan_warning.is_some(),
        "security_warning": startup.trusted_lan_warning,
    });
    let emitted = emit_success(true, CliCommand::RelayStart, data, stdout, stderr);
    if emitted != 0 {
        serving.abort();
        let _ = serving.await;
        return emitted;
    }
    match serving.await {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            if error
                .downcast_ref::<psst_relay::ShutdownTimedOut>()
                .is_some()
            {
                let _ = psst_relay::process_result_for_serve_error(error.as_ref());
                unreachable!("shutdown timeout terminates the process");
            }
            write_bytes(
                stderr,
                b"psst: relay failed after startup\n",
                map_relay_serve_error(error.as_ref()).exit_class().code(),
            )
        }
        Err(_) => write_bytes(stderr, b"psst: relay failed after startup\n", 70),
    }
}

fn handle_relay_error(
    error: &(dyn std::error::Error + 'static),
    hard_exit_on_timeout: bool,
) -> Result<(), LocalErrorCode> {
    if hard_exit_on_timeout
        && error
            .downcast_ref::<psst_relay::ShutdownTimedOut>()
            .is_some()
    {
        let _ = psst_relay::process_result_for_serve_error(error);
        unreachable!("shutdown timeout terminates the process");
    }
    Err(map_relay_serve_error(error))
}

fn map_io_error(error: &std::io::Error) -> LocalErrorCode {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => LocalErrorCode::LocalPermission,
        std::io::ErrorKind::TimedOut => LocalErrorCode::RelayTimeout,
        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AddrNotAvailable => {
            LocalErrorCode::RelayUnavailable
        }
        _ => LocalErrorCode::LocalWrite,
    }
}

fn map_relay_serve_error(error: &(dyn std::error::Error + 'static)) -> LocalErrorCode {
    if error
        .downcast_ref::<psst_relay::ShutdownTimedOut>()
        .is_some()
    {
        return LocalErrorCode::OutcomeUnknown;
    }
    if error.downcast_ref::<psst_relay::ConfigError>().is_some() {
        return LocalErrorCode::InvalidConfiguration;
    }
    if let Some(error) = error.downcast_ref::<std::io::Error>() {
        return match error.kind() {
            std::io::ErrorKind::PermissionDenied => LocalErrorCode::LocalPermission,
            std::io::ErrorKind::TimedOut => LocalErrorCode::RelayTimeout,
            std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AddrNotAvailable => {
                LocalErrorCode::RelayUnavailable
            }
            _ => LocalErrorCode::LocalWrite,
        };
    }
    if let Some(error) = error.downcast_ref::<psst_relay::WorkerError>() {
        return match error {
            psst_relay::WorkerError::RateLimited => LocalErrorCode::RateLimited,
            psst_relay::WorkerError::Timeout => LocalErrorCode::RelayTimeout,
            psst_relay::WorkerError::Unavailable => LocalErrorCode::RelayUnavailable,
            psst_relay::WorkerError::Store => LocalErrorCode::LocalWrite,
        };
    }
    if let Some(error) = error.downcast_ref::<psst_store::StoreError>() {
        return match error {
            psst_store::StoreError::FutureSchema { .. }
            | psst_store::StoreError::MigrationChecksumMismatch { .. }
            | psst_store::StoreError::InvalidMigrationLedger { .. }
            | psst_store::StoreError::InvalidMigrationPlan(_)
            | psst_store::StoreError::UnexpectedApplicationId { .. } => {
                LocalErrorCode::InvalidConfiguration
            }
            psst_store::StoreError::Database(_) => LocalErrorCode::LocalWrite,
            psst_store::StoreError::WorkerUnavailable => LocalErrorCode::RelayUnavailable,
        };
    }
    LocalErrorCode::Internal
}

fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Invocation, ParseError> {
    let mut args: Vec<OsString> = arguments.into_iter().collect();
    if !args.is_empty() {
        args.remove(0);
    }
    let json_requested = leading_json_requested(&args);
    if args.is_empty() || (args.len() == 1 && (args[0] == "--help" || args[0] == "-h")) {
        return Ok(Invocation::Help);
    }
    if args.len() == 1 && (args[0] == "--version" || args[0] == "-V") {
        return Ok(Invocation::Version);
    }
    let mut global = GlobalOptions {
        json: json_requested,
        ..GlobalOptions::default()
    };
    let mut seen_global = std::collections::HashSet::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("--json") if seen_global.insert("--json") => {}
            Some("--relay") => {
                if !seen_global.insert("--relay") {
                    return Err(ParseError {
                        command: None,
                        json: global.json,
                    });
                }
                index += 1;
                global.relay = Some(string_arg(args.get(index), global.json)?);
            }
            Some("--profile") => {
                if !seen_global.insert("--profile") {
                    return Err(ParseError {
                        command: None,
                        json: global.json,
                    });
                }
                index += 1;
                global.profile = Some(string_arg(args.get(index), global.json)?);
            }
            Some("--config") => {
                if !seen_global.insert("--config") {
                    return Err(ParseError {
                        command: None,
                        json: global.json,
                    });
                }
                index += 1;
                global.config = Some(PathBuf::from(os_arg(args.get(index), global.json)?));
            }
            Some("--json") => {
                return Err(ParseError {
                    command: None,
                    json: true,
                });
            }
            _ => break,
        }
        index += 1;
    }
    let rest = &args[index..];
    let command = parse_command(rest, global.json)?;
    Ok(Invocation::Command(global, command))
}

fn leading_json_requested(args: &[OsString]) -> bool {
    let mut index = 0;
    let mut json = false;
    while index < args.len() {
        match args[index].to_str() {
            Some("--json") => {
                json = true;
                index += 1;
            }
            Some("--relay" | "--profile" | "--config") => {
                let Some(value) = args.get(index + 1).and_then(|value| value.to_str()) else {
                    break;
                };
                if value.starts_with('-') {
                    break;
                }
                index += 2;
            }
            _ => break,
        }
    }
    json
}

fn parse_command(args: &[OsString], json: bool) -> Result<ParsedCommand, ParseError> {
    let Some(head) = args.first().and_then(|value| value.to_str()) else {
        return Err(ParseError {
            command: None,
            json,
        });
    };
    match head {
        "health" if args.len() == 1 => Ok(ParsedCommand::Health),
        "relay" if args.get(1).is_some_and(|value| value == "start") => {
            parse_relay_start(&args[2..], json)
        }
        "agent" if args.get(1).is_some_and(|value| value == "claude") => {
            parse_agent_claude(&args[2..], json)
        }
        "agent" if args.get(1).is_some_and(|value| value == "codex") => {
            parse_agent_codex(&args[2..], json)
        }
        "agent" if args.get(1).is_some_and(|value| value == "status") && args.len() == 2 => {
            Ok(ParsedCommand::AgentStatus)
        }
        "internal"
            if args.get(1).is_some_and(|value| value == "mcp") && args.len() == 2 && !json =>
        {
            Ok(ParsedCommand::InternalMcp)
        }
        "config"
            if args.get(1).is_some_and(|value| value == "show")
                && args.get(2).is_some_and(|value| value == "--effective")
                && args.len() == 3 =>
        {
            Ok(ParsedCommand::ConfigShowEffective)
        }
        "harness" if args.get(1).is_some_and(|value| value == "status") && args.len() == 2 => {
            Ok(ParsedCommand::HarnessStatus)
        }
        "squad" if args.get(1).is_some_and(|value| value == "list") && args.len() == 2 => {
            Ok(ParsedCommand::SquadList)
        }
        "squad" if args.get(1).is_some_and(|value| value == "create") => {
            let command = CliCommand::SquadCreate;
            if args.len() != 5 || args.get(3).is_none_or(|value| value != "--mission") {
                return Err(ParseError {
                    command: Some(command),
                    json,
                });
            }
            Ok(ParsedCommand::SquadCreate {
                squad: command_arg(args.get(2), json, command)?,
                mission: command_arg(args.get(4), json, command)?,
            })
        }
        "squad" if args.get(1).is_some_and(|value| value == "describe") && args.len() == 3 => {
            Ok(ParsedCommand::SquadDescribe {
                squad: command_arg(args.get(2), json, CliCommand::SquadDescribe)?,
            })
        }
        _ => parse_deferred(args, json),
    }
}

fn parse_agent_claude(args: &[OsString], json: bool) -> Result<ParsedCommand, ParseError> {
    let command = CliCommand::AgentClaude;
    let mut parsed = AgentClaudeArgs::default();
    for argument in args {
        match argument.to_str() {
            Some("--continue") if !parsed.continue_session => parsed.continue_session = true,
            Some("--dangerously-skip-permissions") if !parsed.dangerously_skip_permissions => {
                parsed.dangerously_skip_permissions = true;
            }
            _ => {
                return Err(ParseError {
                    command: Some(command),
                    json,
                });
            }
        }
    }
    Ok(ParsedCommand::AgentClaude(parsed))
}

fn parse_agent_codex(args: &[OsString], json: bool) -> Result<ParsedCommand, ParseError> {
    let command = CliCommand::AgentCodex;
    let mut parsed = AgentCodexArgs::default();
    for argument in args {
        match argument.to_str() {
            Some("--continue") if !parsed.continue_session => parsed.continue_session = true,
            _ => {
                return Err(ParseError {
                    command: Some(command),
                    json,
                });
            }
        }
    }
    Ok(ParsedCommand::AgentCodex(parsed))
}

fn parse_relay_start(args: &[OsString], json: bool) -> Result<ParsedCommand, ParseError> {
    let command = CliCommand::RelayStart;
    let mut parsed = RelayStartArgs::default();
    let mut seen = std::collections::HashSet::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("--bind") => {
                if !seen.insert("--bind") {
                    return Err(ParseError {
                        command: Some(command),
                        json,
                    });
                }
                index += 1;
                parsed.bind = Some(
                    command_arg(args.get(index), json, command)?
                        .parse()
                        .map_err(|_| ParseError {
                            command: Some(command),
                            json,
                        })?,
                );
            }
            Some("--data-dir") => {
                if !seen.insert("--data-dir") {
                    return Err(ParseError {
                        command: Some(command),
                        json,
                    });
                }
                index += 1;
                parsed.data_dir = Some(PathBuf::from(command_os_arg(
                    args.get(index),
                    json,
                    command,
                )?));
            }
            Some("--allow-lan") if seen.insert("--allow-lan") => parsed.allow_lan = true,
            Some("--log") => {
                if !seen.insert("--log") {
                    return Err(ParseError {
                        command: Some(command),
                        json,
                    });
                }
                index += 1;
                parsed.log_level = Some(command_arg(args.get(index), json, command)?);
            }
            _ => {
                return Err(ParseError {
                    command: Some(command),
                    json,
                });
            }
        }
        index += 1;
    }
    Ok(ParsedCommand::RelayStart(parsed))
}

fn parse_deferred(args: &[OsString], json: bool) -> Result<ParsedCommand, ParseError> {
    let key = args
        .iter()
        .take(2)
        .filter_map(|value| value.to_str())
        .collect::<Vec<_>>()
        .join(" ");
    let command = match key.as_str() {
        "profile list" if args.len() == 2 => CliCommand::ProfileList,
        "profile show"
            if args.len() == 2 || (args.len() == 3 && nonempty(args.get(2)).is_some()) =>
        {
            CliCommand::ProfileShow
        }
        "squad archive" if args.len() == 3 && nonempty(args.get(2)).is_some() => {
            CliCommand::SquadArchive
        }
        "squad join" if valid_join_grammar(args) => CliCommand::SquadJoin,
        "squad leave" if args.len() == 2 => CliCommand::SquadLeave,
        "squad roster" if args.len() == 2 => CliCommand::SquadRoster,
        "message send" if valid_send_grammar(args) => CliCommand::MessageSend,
        "message acknowledge"
            if args.len() >= 3
                && args.len() <= 102
                && args[2..]
                    .iter()
                    .all(|value| nonempty(Some(value)).is_some()) =>
        {
            CliCommand::MessageAcknowledge
        }
        "database info" if args.len() == 2 => CliCommand::DatabaseInfo,
        "database backup" if args.len() == 3 && nonempty(args.get(2)).is_some() => {
            CliCommand::DatabaseBackup
        }
        "database integrity-check" if args.len() == 2 => CliCommand::DatabaseIntegrityCheck,
        _ if args.first().is_some_and(|value| value == "inbox") && valid_inbox_grammar(args) => {
            CliCommand::Inbox
        }
        _ if args.first().is_some_and(|value| value == "listen") && valid_listen_grammar(args) => {
            CliCommand::Listen
        }
        _ if args.first().is_some_and(|value| value == "transcript")
            && valid_transcript_grammar(args) =>
        {
            CliCommand::Transcript
        }
        _ if args.first().is_some_and(|value| value == "status") && args.len() == 1 => {
            CliCommand::Status
        }
        _ => {
            let command = deferred_identity(args);
            return Err(ParseError { command, json });
        }
    };
    Ok(ParsedCommand::Deferred {
        command,
        arguments: args.to_vec(),
    })
}

fn deferred_identity(args: &[OsString]) -> Option<CliCommand> {
    let first = args.first()?.to_str()?;
    let second = args.get(1).and_then(|value| value.to_str());
    match (first, second) {
        ("relay", Some("start")) => Some(CliCommand::RelayStart),
        ("agent", Some("claude")) => Some(CliCommand::AgentClaude),
        ("agent", Some("codex")) => Some(CliCommand::AgentCodex),
        ("agent", Some("status")) => Some(CliCommand::AgentStatus),
        ("health", _) => Some(CliCommand::Health),
        ("config", Some("show")) => Some(CliCommand::ConfigShowEffective),
        ("harness", Some("status")) => Some(CliCommand::HarnessStatus),
        ("profile", Some("list")) => Some(CliCommand::ProfileList),
        ("profile", Some("show")) => Some(CliCommand::ProfileShow),
        ("squad", Some("archive")) => Some(CliCommand::SquadArchive),
        ("squad", Some("list")) => Some(CliCommand::SquadList),
        ("squad", Some("create")) => Some(CliCommand::SquadCreate),
        ("squad", Some("describe")) => Some(CliCommand::SquadDescribe),
        ("squad", Some("join")) => Some(CliCommand::SquadJoin),
        ("squad", Some("leave")) => Some(CliCommand::SquadLeave),
        ("squad", Some("roster")) => Some(CliCommand::SquadRoster),
        ("message", Some("send")) => Some(CliCommand::MessageSend),
        ("message", Some("acknowledge")) => Some(CliCommand::MessageAcknowledge),
        ("database", Some("info")) => Some(CliCommand::DatabaseInfo),
        ("database", Some("backup")) => Some(CliCommand::DatabaseBackup),
        ("database", Some("integrity-check")) => Some(CliCommand::DatabaseIntegrityCheck),
        ("inbox", _) => Some(CliCommand::Inbox),
        ("listen", _) => Some(CliCommand::Listen),
        ("transcript", _) => Some(CliCommand::Transcript),
        ("status", _) => Some(CliCommand::Status),
        _ => None,
    }
}

fn valid_join_grammar(args: &[OsString]) -> bool {
    if args.len() < 7 || nonempty(args.get(2)).is_none() {
        return false;
    }
    valid_named_options(&args[3..], &["--name", "--role"], &["--mission"])
}

fn valid_send_grammar(args: &[OsString]) -> bool {
    let Some(options) = parse_option_pairs(
        &args[2..],
        &[
            "--to",
            "--body",
            "--file",
            "--priority",
            "--reply-to",
            "--correlation-id",
        ],
    ) else {
        return false;
    };
    options.contains_key("--to")
        && (options.contains_key("--body") ^ options.contains_key("--file"))
        && options
            .get("--priority")
            .is_none_or(|value| matches!(value.as_str(), "normal" | "high"))
}

fn valid_inbox_grammar(args: &[OsString]) -> bool {
    let mut index = 1;
    let mut seen = std::collections::HashSet::new();
    while index < args.len() {
        let Some(flag) = args[index].to_str() else {
            return false;
        };
        if !seen.insert(flag) {
            return false;
        }
        match flag {
            "--limit" => {
                index += 1;
                if !bounded_number(args.get(index), 1, 100) {
                    return false;
                }
            }
            "--wait" => {
                index += 1;
                if !bounded_number(args.get(index), 0, 30) {
                    return false;
                }
            }
            "--ack" => {
                index += 1;
                let start = index;
                while index < args.len() && !args[index].to_string_lossy().starts_with("--") {
                    if nonempty(args.get(index)).is_none() {
                        return false;
                    }
                    index += 1;
                }
                if index == start {
                    return false;
                }
                if index - start > 100 {
                    return false;
                }
                continue;
            }
            _ => return false,
        }
        index += 1;
    }
    true
}

fn valid_listen_grammar(args: &[OsString]) -> bool {
    let mut index = 1;
    let mut wait = false;
    let mut ack = false;
    while index < args.len() {
        match args[index].to_str() {
            Some("--wait") if !wait => {
                wait = true;
                index += 1;
                if !bounded_number(args.get(index), 1, 30) {
                    return false;
                }
            }
            Some("--ack") if !ack => ack = true,
            _ => return false,
        }
        index += 1;
    }
    true
}

fn valid_transcript_grammar(args: &[OsString]) -> bool {
    let Some(options) = parse_option_pairs(&args[1..], &["--after", "--limit"]) else {
        return false;
    };
    options
        .get("--after")
        .is_none_or(|value| value.parse::<i64>().is_ok_and(|number| number >= 0))
        && options.get("--limit").is_none_or(|value| {
            value
                .parse::<u16>()
                .is_ok_and(|number| (1..=100).contains(&number))
        })
}

fn valid_named_options(args: &[OsString], required: &[&str], optional: &[&str]) -> bool {
    let allowed = required.iter().chain(optional).copied().collect::<Vec<_>>();
    parse_option_pairs(args, &allowed)
        .is_some_and(|values| required.iter().all(|flag| values.contains_key(*flag)))
}

fn parse_option_pairs(args: &[OsString], allowed: &[&str]) -> Option<BTreeMap<String, String>> {
    if !args.len().is_multiple_of(2) {
        return None;
    }
    let mut values = BTreeMap::new();
    for pair in args.chunks_exact(2) {
        let flag = pair[0].to_str()?;
        let value = nonempty(pair.get(1))?;
        if !allowed.contains(&flag) || values.insert(flag.to_owned(), value).is_some() {
            return None;
        }
    }
    Some(values)
}

fn bounded_number(value: Option<&OsString>, min: u16, max: u16) -> bool {
    nonempty(value)
        .and_then(|value| value.parse::<u16>().ok())
        .is_some_and(|value| (min..=max).contains(&value))
}

fn nonempty(value: Option<&OsString>) -> Option<String> {
    value?
        .to_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn string_arg(value: Option<&OsString>, json: bool) -> Result<String, ParseError> {
    value
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(ParseError {
            command: None,
            json,
        })
}

fn os_arg(value: Option<&OsString>, json: bool) -> Result<&OsStr, ParseError> {
    value
        .filter(|value| !value.is_empty())
        .map(OsString::as_os_str)
        .ok_or(ParseError {
            command: None,
            json,
        })
}

fn command_arg(
    value: Option<&OsString>,
    json: bool,
    command: CliCommand,
) -> Result<String, ParseError> {
    string_arg(value, json).map_err(|_| ParseError {
        command: Some(command),
        json,
    })
}

fn command_os_arg(
    value: Option<&OsString>,
    json: bool,
    command: CliCommand,
) -> Result<&OsStr, ParseError> {
    os_arg(value, json).map_err(|_| ParseError {
        command: Some(command),
        json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use psst_application::ProfilePaths;
    use std::time::Duration;

    #[test]
    fn local_send_capacity_maps_to_local_lock_not_relay_database_busy() {
        assert_eq!(
            map_session_error(&SessionError::SendCapacity),
            LocalErrorCode::LocalLock
        );
    }

    #[tokio::test]
    async fn failed_leave_awaits_shutdown_without_masking_the_leave_error() {
        let shutdown_awaited = std::sync::atomic::AtomicBool::new(false);
        let result = finish_leave(Err(SessionError::NotReady), async {
            shutdown_awaited.store(true, std::sync::atomic::Ordering::SeqCst);
            Err(SessionError::ShutdownTimedOut)
        })
        .await;
        assert_eq!(result.unwrap_err(), LocalErrorCode::InvalidSession);
        assert!(shutdown_awaited.load(std::sync::atomic::Ordering::SeqCst));
    }

    fn write_restricted(path: &std::path::Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        #[cfg(windows)]
        let mut file = psst_platform_security::create_restricted_file(
            path,
            &psst_platform_security::current_process_sid().unwrap(),
        )
        .unwrap();
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt as _;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .unwrap()
        };
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }

    async fn invoke(args: &[&str]) -> (u8, String, String) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_with_io(args.iter().map(OsString::from), &mut stdout, &mut stderr).await;
        (
            code,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    }

    #[tokio::test]
    async fn help_and_version_are_exact() {
        let (code, stdout, stderr) = invoke(&["psst", "--help"]).await;
        assert_eq!(code, 0);
        assert_eq!(stdout, CLI_HELP);
        assert!(stderr.is_empty());
        let (code, stdout, stderr) = invoke(&["psst", "--version"]).await;
        assert_eq!(code, 0);
        assert_eq!(stdout, format!("psst {VERSION}\n"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn unified_agent_grammar_is_closed_and_internal_mcp_is_hidden() {
        let Invocation::Command(_, ParsedCommand::AgentClaude(claude)) =
            parse(["psst", "agent", "claude", "--continue"].map(OsString::from)).unwrap()
        else {
            panic!("Claude agent command was not selected");
        };
        assert!(claude.continue_session);
        assert!(!claude.dangerously_skip_permissions);

        let Invocation::Command(_, ParsedCommand::AgentCodex(codex)) =
            parse(["psst", "agent", "codex", "--continue"].map(OsString::from)).unwrap()
        else {
            panic!("Codex agent command was not selected");
        };
        assert!(codex.continue_session);

        assert!(parse(["psst", "agent", "claude", "--unknown"].map(OsString::from)).is_err());
        assert!(
            parse(["psst", "agent", "codex", "--dangerously-skip-permissions"].map(OsString::from))
                .is_err()
        );
        assert!(parse(["psst", "--json", "internal", "mcp"].map(OsString::from)).is_err());
        assert!(CLI_HELP.contains("agent claude [--continue]"));
        assert!(!CLI_HELP.contains("internal mcp"));
    }

    #[test]
    fn generated_claude_config_uses_the_unified_binary_and_contains_no_authority() {
        let directory = tempfile::tempdir().unwrap();
        let platform = PlatformPaths {
            config_dir: directory.path().join("config"),
            data_dir: directory.path().join("data"),
            runtime_dir: directory.path().join("runtime"),
        };
        let resolved = ConfigResolver::new(platform)
            .resolve(&ConfigInputs {
                flags: ConfigFlags {
                    relay_origin: Some("http://relay.tailnet:7341".into()),
                    profile: Some("research-claude".into()),
                    config_path: Some(directory.path().join("missing.yaml")),
                    ..ConfigFlags::default()
                },
                environment: BTreeMap::new(),
            })
            .unwrap();
        let output = directory.path().join("agent");
        std::fs::create_dir_all(&output).unwrap();
        let executable = directory.path().join("psst.exe");
        let file = write_claude_mcp_config(&resolved, &executable, "psst-research-claude", &output)
            .unwrap();
        let value: Value = serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
        let server = &value["mcpServers"]["psst-research-claude"];
        assert_eq!(server["command"], executable.to_string_lossy().as_ref());
        assert_eq!(server["args"], serde_json::json!(["internal", "mcp"]));
        assert_eq!(server["env"]["PSST_PROFILE"], "research-claude");
        assert_eq!(server["env"]["PSST_RELAY"], "http://relay.tailnet:7341");
        let encoded = value.to_string().to_ascii_lowercase();
        for forbidden in ["authorization", "bearer", "credential", "token", "secret"] {
            assert!(!encoded.contains(forbidden));
        }
        let path = file.path().to_owned();
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn agent_launcher_lock_is_exclusive_and_reusable() {
        let directory = tempfile::tempdir().unwrap();
        let first = AgentLaunchGuard::acquire(directory.path()).unwrap();
        assert!(AgentLaunchGuard::acquire(directory.path()).is_err());
        drop(first);
        AgentLaunchGuard::acquire(directory.path()).unwrap();
    }

    #[test]
    fn codex_thread_record_is_bounded_and_exact() {
        let directory = tempfile::tempdir().unwrap();
        let record = directory.path().join("codex-thread-id");
        assert_eq!(load_codex_thread_record(&record), Ok(None));
        std::fs::write(&record, b"thr_durable\n").unwrap();
        assert_eq!(
            load_codex_thread_record(&record),
            Ok(Some("thr_durable".into()))
        );
        std::fs::write(&record, vec![b'x'; 130]).unwrap();
        assert_eq!(load_codex_thread_record(&record), Err(()));
        std::fs::write(&record, b"bad value\n").unwrap();
        assert_eq!(load_codex_thread_record(&record), Err(()));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_command_shim_preserves_following_agent_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("arguments.txt");
        let shim = directory.path().join("agent.cmd");
        std::fs::write(
            &shim,
            format!("@echo off\r\n@echo %* > \"{}\"\r\n", output.display()),
        )
        .unwrap();
        let (command, prefix) = executable_command(shim).unwrap();
        let status = Command::new(command)
            .args(prefix)
            .args(["app-server", "--stdio"])
            .status()
            .await
            .unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(output).unwrap().trim(),
            "app-server --stdio"
        );
    }

    #[test]
    fn harness_status_reads_the_shared_nonsecret_record_without_profile_ownership() {
        let directory = tempfile::tempdir().unwrap();
        let platform = PlatformPaths {
            config_dir: directory.path().join("config"),
            data_dir: directory.path().join("state"),
            runtime_dir: directory.path().join("runtime"),
        };
        let origin = "http://127.0.0.1:7341";
        let resolved = ConfigResolver::new(platform.clone())
            .resolve(&ConfigInputs {
                flags: ConfigFlags {
                    relay_origin: Some(origin.into()),
                    profile: Some("alpha".into()),
                    config_path: Some(directory.path().join("missing.yaml")),
                    ..ConfigFlags::default()
                },
                environment: BTreeMap::new(),
            })
            .unwrap();
        let paths = ProfilePaths::for_profile(&platform, origin, "alpha").unwrap();
        write_restricted(
            &harness_status_path(&paths).unwrap(),
            br#"{"version":1,"profile":"alpha","adapter":"codex_app_server","phase":"quiet","retry_attempt":0,"pending_count":null,"highest_priority":null,"owner_pid":7,"observed_at":"1970-01-01T00:00:00.000Z"}"#,
        );

        let value = execute_harness_status(&resolved).unwrap();
        assert_eq!(value["profile"], "alpha");
        assert_eq!(value["adapter"], "codex_app_server");
        assert_eq!(value["phase"], "quiet");
        assert_eq!(value["freshness"], "stale");
        assert_eq!(value["pending_count"], Value::Null);
        let encoded = value.to_string().to_ascii_lowercase();
        for forbidden in ["authorization", "bearer", "credential", "token", "body"] {
            assert!(!encoded.contains(forbidden));
        }
    }

    #[tokio::test]
    async fn missing_metadata_replays_confirmed_leave_and_fails_closed_on_intent() {
        for phase in ["confirmed", "intent"] {
            let directory = tempfile::tempdir().unwrap();
            let origin = "http://127.0.0.1:9";
            let platform_paths = PlatformPaths {
                config_dir: directory.path().join("config"),
                data_dir: directory.path().join("state"),
                runtime_dir: directory.path().join("runtime"),
            };
            let resolved = ConfigResolver::new(platform_paths.clone())
                .resolve(&ConfigInputs {
                    flags: ConfigFlags {
                        config_path: Some(directory.path().join("missing.yaml")),
                        relay_origin: Some(origin.into()),
                        profile: Some("default".into()),
                        ..ConfigFlags::default()
                    },
                    environment: BTreeMap::new(),
                })
                .unwrap();
            let paths = ProfilePaths::for_profile(&platform_paths, origin, "default").unwrap();
            let journal = paths.metadata.with_file_name(format!(
                "{}.leave-v1.json",
                paths.metadata.file_stem().unwrap().to_string_lossy()
            ));
            let confirmed_at = (phase == "confirmed")
                .then_some(serde_json::json!("2026-08-08T01:02:04.005Z"))
                .unwrap_or(serde_json::Value::Null);
            let record = serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "phase": phase,
                "relay_origin": origin,
                "profile": "default",
                "squad_name": "alpha",
                "squad_id": "sqd_alpha",
                "member_id": "mem_worker",
                "operation_id": "fixed-operation-0",
                "created_at": "2026-08-08T01:02:03.004Z",
                "confirmed_at": confirmed_at,
            }))
            .unwrap();
            write_restricted(&journal, &record);

            let result = open_session(&resolved).await;
            if phase == "confirmed" {
                assert!(matches!(result, Err(LocalErrorCode::ProfileUnbound)));
                assert!(!journal.exists());
            } else {
                assert!(matches!(result, Err(LocalErrorCode::OutcomeUnknown)));
                assert!(journal.exists());
            }
            assert!(!paths.metadata.exists());
        }
    }

    #[tokio::test]
    async fn json_failure_uses_only_stderr_and_stable_exit() {
        let (code, stdout, stderr) =
            invoke(&["psst", "--json", "squad", "create", "missing-mission"]).await;
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(
            serde_json::from_str::<Value>(&stderr).unwrap(),
            serde_json::json!({"version":"psst.cli.v1","ok":false,"command":"squad_create","error":{"code":"invalid_input","message":"The request is invalid.","retryable":false,"exit_class":"usage"}})
        );

        let (code, stdout, stderr) = invoke(&["psst", "--json", "relay", "start", "--bind"]).await;
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(
            serde_json::from_str::<Value>(&stderr).unwrap()["command"],
            "relay_start"
        );

        let (code, stdout, stderr) = invoke(&["psst", "--json", "unknown"]).await;
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(
            serde_json::from_str::<Value>(&stderr).unwrap()["command"],
            "invocation"
        );

        let (code, stdout, stderr) = invoke(&["psst", "unknown", "--json"]).await;
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(stderr, "psst: invalid command line\n");

        let (code, stdout, stderr) = invoke(&[
            "psst", "message", "send", "--to", "worker", "--body", "--json",
        ])
        .await;
        assert_ne!(
            code, 2,
            "--json is the message body, not a late global flag"
        );
        assert!(stdout.is_empty());
        assert!(stderr.starts_with("psst: "));
    }

    #[tokio::test]
    async fn every_w305_command_has_an_exact_usage_failure_envelope() {
        let cases: &[(&[&str], &str)] = &[
            (&["psst", "--json", "squad", "archive"], "squad_archive"),
            (
                &["psst", "--json", "squad", "join", "alpha", "--name", "a"],
                "squad_join",
            ),
            (
                &["psst", "--json", "squad", "leave", "extra"],
                "squad_leave",
            ),
            (
                &["psst", "--json", "squad", "roster", "extra"],
                "squad_roster",
            ),
            (
                &["psst", "--json", "message", "send", "--to", "b"],
                "message_send",
            ),
            (&["psst", "--json", "inbox", "--limit", "0"], "inbox"),
            (&["psst", "--json", "listen", "--wait", "0"], "listen"),
            (
                &["psst", "--json", "message", "acknowledge"],
                "message_acknowledge",
            ),
            (
                &["psst", "--json", "transcript", "--after", "-1"],
                "transcript",
            ),
            (&["psst", "--json", "status", "extra"], "status"),
            (
                &["psst", "--json", "harness", "status", "extra"],
                "harness_status",
            ),
        ];
        for (arguments, command) in cases {
            let (code, stdout, stderr) = invoke(arguments).await;
            assert_eq!(code, 2, "{command}");
            assert!(stdout.is_empty(), "{command}");
            assert_eq!(
                serde_json::from_str::<Value>(&stderr).unwrap(),
                serde_json::json!({
                    "version":"psst.cli.v1","ok":false,"command":command,
                    "error":{"code":"invalid_input","message":"The request is invalid.","retryable":false,"exit_class":"usage"}
                }),
                "{command}"
            );
        }
    }

    #[test]
    fn representative_human_messaging_output_is_stable() {
        let mut output = Vec::new();
        assert_eq!(
            write_human(
                CliCommand::MessageSend,
                &serde_json::json!({"message":{"id":"msg_one","body":"hello"},"idempotent_replay":false}),
                &mut output,
            ),
            0
        );
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\n  \"idempotent_replay\": false,\n  \"message\": {\n    \"body\": \"hello\",\n    \"id\": \"msg_one\"\n  }\n}\n"
        );
    }

    #[tokio::test]
    async fn grammar_failures_precede_input_and_explicit_stdin_does_not_hang_without_a_session() {
        let (code, stdout, stderr) = invoke(&[
            "psst", "--json", "message", "send", "--to", "worker", "--file", "-",
        ])
        .await;
        assert_ne!(code, 0);
        assert!(stdout.is_empty());
        assert!(!stderr.to_ascii_lowercase().contains("bearer "));

        for invalid in [
            vec!["psst", "--json", "message", "send", "--to", "worker"],
            vec!["psst", "--json", "squad", "join", "alpha", "--name", "one"],
            vec!["psst", "--json", "inbox", "--limit", "101"],
            vec!["psst", "--json", "listen", "--ack", "extra"],
            vec![
                "psst",
                "--json",
                "transcript",
                "--after",
                "9223372036854775808",
            ],
            vec!["psst", "--json", "database", "backup"],
        ] {
            let (code, stdout, stderr) = invoke(&invalid).await;
            assert_eq!(code, 2, "{invalid:?}");
            assert!(stdout.is_empty(), "{invalid:?}");
            assert_eq!(
                serde_json::from_str::<Value>(&stderr).unwrap()["error"]["code"],
                "invalid_input",
                "{invalid:?}"
            );
        }

        let mut oversized_ack = vec![OsString::from("message"), OsString::from("acknowledge")];
        oversized_ack.extend((0..101).map(|index| OsString::from(format!("msg_{index}"))));
        assert!(parse_deferred(&oversized_ack, true).is_err());

        let mut oversized_inbox_ack = vec![OsString::from("inbox"), OsString::from("--ack")];
        oversized_inbox_ack.extend((0..101).map(|index| OsString::from(format!("msg_{index}"))));
        assert!(parse_deferred(&oversized_inbox_ack, true).is_err());
    }

    #[test]
    fn explicit_input_reader_accepts_exact_bound_and_rejects_oversize_and_invalid_utf8() {
        assert_eq!(
            read_bounded_utf8(io::Cursor::new(b"12345"), 5),
            Ok("12345".into())
        );
        assert_eq!(
            read_bounded_utf8(io::Cursor::new(b"123456"), 5),
            Err(LocalErrorCode::PayloadTooLarge)
        );
        assert_eq!(
            read_bounded_utf8(io::Cursor::new([0xff]), 5),
            Err(LocalErrorCode::InvalidInput)
        );
    }

    #[tokio::test]
    async fn relay_validation_precedes_any_filesystem_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().join("must-not-exist");
        let paths = PlatformPaths {
            config_dir: directory.path().join("config"),
            data_dir: directory.path().join("state"),
            runtime_dir: directory.path().join("runtime"),
        };
        let mut resolved = ConfigResolver::new(paths)
            .resolve(&ConfigInputs {
                flags: ConfigFlags {
                    config_path: Some(directory.path().join("missing.yaml")),
                    data_dir: Some(data_dir.clone()),
                    ..ConfigFlags::default()
                },
                environment: BTreeMap::new(),
            })
            .unwrap();
        resolved.log_level.value = "[invalid".into();
        let error = run_relay_until(true, false, &resolved, async {}).await;
        assert_eq!(error, Err(LocalErrorCode::InvalidConfiguration));
        assert!(!data_dir.exists());
    }

    #[tokio::test]
    async fn injectable_primary_shutdown_branch_completes_without_broadcasting_a_signal() {
        assert!(
            first_shutdown_signal(std::future::ready(()), std::future::pending::<Option<()>>())
                .await
        );
    }

    #[tokio::test]
    async fn unavailable_signal_sources_do_not_request_shutdown() {
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                wait_for_successful_signal(std::future::ready(Err::<(), ()>(())))
            )
            .await
            .is_err()
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                wait_for_present_signal(std::future::ready(None::<()>))
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn effective_configuration_is_versioned_redacted_and_tracks_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.yaml");
        std::fs::write(
            &config,
            "relay_origin: http://127.0.0.1:7441\nprofile: file-profile\nlog_level: warn\n",
        )
        .unwrap();
        let (code, stdout, stderr) = invoke(&[
            "psst",
            "--json",
            "--config",
            config.to_str().unwrap(),
            "--relay",
            "http://127.0.0.1:7551",
            "config",
            "show",
            "--effective",
        ])
        .await;
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let document: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(document["version"], "psst.cli.v1");
        assert_eq!(document["command"], "config_show_effective");
        assert_eq!(
            document["data"]["relay_origin"]["value"],
            "http://127.0.0.1:7551"
        );
        assert_eq!(document["data"]["relay_origin"]["source"], "command_line");
        assert_eq!(document["data"]["profile"]["value"], "file-profile");
        assert_eq!(document["data"]["profile"]["source"], "config_file");
        assert_eq!(document["data"]["log_level"]["source"], "config_file");
        assert!(matches!(
            document["data"]["credential_state"].as_str(),
            Some("absent" | "available" | "unavailable")
        ));
        let serialized = stdout.to_ascii_lowercase();
        assert!(!serialized.contains("bearer "));
        assert!(!serialized.contains("resume_token"));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One ordered real-runtime journey verifies the CLI boundary.
    async fn real_relay_health_and_unauthenticated_squad_commands_obey_json_contract() {
        let directory = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let paths = PlatformPaths {
            config_dir: directory.path().join("config"),
            data_dir: directory.path().join("state"),
            runtime_dir: directory.path().join("runtime"),
        };
        let resolved = ConfigResolver::new(paths)
            .resolve(&ConfigInputs {
                flags: ConfigFlags {
                    config_path: Some(directory.path().join("missing.yaml")),
                    relay_origin: Some(format!("http://{address}")),
                    relay_bind: Some(format!("0.0.0.0:{}", address.port())),
                    data_dir: Some(directory.path().join("relay")),
                    allow_lan: Some("true".into()),
                    ..ConfigFlags::default()
                },
                environment: BTreeMap::new(),
            })
            .unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let relay = tokio::spawn({
            let resolved = resolved.clone();
            async move {
                run_relay_until(true, false, &resolved, async move {
                    let _ = stop_rx.await;
                })
                .await
            }
        });
        let client = Client::new(&format!("http://{address}"), ClientConfig::default()).unwrap();
        for _ in 0..100 {
            if client.health().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        client.health().await.unwrap();
        let config_path = directory.path().join("missing.yaml");
        let origin = format!("http://{address}");
        let base = [
            "psst",
            "--json",
            "--config",
            config_path.to_str().unwrap(),
            "--relay",
            origin.as_str(),
        ];
        let mut health = base.to_vec();
        health.push("health");
        let (code, stdout, stderr) = invoke(&health).await;
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let health: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(health["data"]["health"]["status"], "ok");
        assert_eq!(health["data"]["ready"]["status"], "ready");

        let mut create = base.to_vec();
        create.extend(["squad", "create", "alpha", "--mission", "verify cli"]);
        let (code, stdout, stderr) = invoke(&create).await;
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let created: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(created["command"], "squad_create");
        assert_eq!(created["data"]["name"], "alpha");

        let (code, stdout, stderr) = invoke(&create).await;
        assert_eq!(code, 5);
        assert!(stdout.is_empty());
        let duplicate: Value = serde_json::from_str(&stderr).unwrap();
        assert_eq!(duplicate["error"]["code"], "name_in_use");

        let mut describe = base.to_vec();
        describe.extend(["squad", "describe", "alpha"]);
        let (code, stdout, stderr) = invoke(&describe).await;
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert_eq!(
            serde_json::from_str::<Value>(&stdout).unwrap()["data"]["mission"],
            "verify cli"
        );

        let mut list = base.to_vec();
        list.extend(["squad", "list"]);
        let (code, stdout, stderr) = invoke(&list).await;
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert_eq!(
            serde_json::from_str::<Value>(&stdout).unwrap()["data"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        stop_tx.send(()).unwrap();
        let stopped = tokio::time::timeout(Duration::from_secs(5), relay)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(stopped["shutdown"]["clean"], true);
        assert!(directory.path().join("relay/psst.db").exists());
    }

    #[test]
    fn cli_hard_timeout_uses_the_relay_immediate_exit_path() {
        const CHILD: &str = "PSST_CLI_TEST_HARD_TIMEOUT_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let _wedged = std::thread::spawn(|| {
                loop {
                    std::thread::park();
                }
            });
            handle_relay_error(&psst_relay::ShutdownTimedOut, true).unwrap();
            unreachable!();
        }
        let started = std::time::Instant::now();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::cli_hard_timeout_uses_the_relay_immediate_exit_path",
            ])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(3));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
