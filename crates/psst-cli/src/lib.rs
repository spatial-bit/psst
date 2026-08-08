//! Human and operator command shell for Psst.

#![forbid(unsafe_code)]

use psst_application::{
    CLI_HELP, CliCommand, CliFailure, CliSuccess, ConfigFlags, ConfigInputs, ConfigResolver,
    CredentialState, LocalErrorCode, PlatformPaths, ResolvedConfig, emit_json_failure,
    emit_json_success, map_client_error,
};
use psst_client::{Client, ClientConfig};
use psst_protocol::CreateSquadRequest;
use serde::Serialize;
use serde_json::Value;
use std::fmt::Write as _;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    future::Future,
    io::{self, Write},
    net::SocketAddr,
    path::PathBuf,
    process::ExitCode,
};

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
    Health,
    ConfigShowEffective,
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
            Self::Health => CliCommand::Health,
            Self::ConfigShowEffective => CliCommand::ConfigShowEffective,
            Self::SquadList => CliCommand::SquadList,
            Self::SquadCreate { .. } => CliCommand::SquadCreate,
            Self::SquadDescribe { .. } => CliCommand::SquadDescribe,
            Self::Deferred { command, .. } => *command,
        }
    }
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
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let code = run_with_io(arguments, &mut stdout, &mut stderr).await;
    ExitCode::from(code)
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
        ParsedCommand::Deferred { arguments, .. } => {
            let _ = arguments;
            Err(LocalErrorCode::Unsupported)
        }
    }
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
        let _ = tokio::signal::ctrl_c().await;
        return;
    };
    // Interactive Ctrl-C is supported. The child-process test uses targeted Ctrl-Break because
    // Windows cannot safely target CTRL_C_EVENT at one process without broadcasting to its console.
    let _ = first_shutdown_signal(tokio::signal::ctrl_c(), ctrl_break.recv()).await;
}

#[cfg(not(windows))]
async fn shutdown_signal() {
    // Tokio's Ctrl-C future is backed by SIGINT on Unix.
    let _ = tokio::signal::ctrl_c().await;
}

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
        "config"
            if args.get(1).is_some_and(|value| value == "show")
                && args.get(2).is_some_and(|value| value == "--effective")
                && args.len() == 3 =>
        {
            Ok(ParsedCommand::ConfigShowEffective)
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
        ("health", _) => Some(CliCommand::Health),
        ("config", Some("show")) => Some(CliCommand::ConfigShowEffective),
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
        .is_none_or(|value| value.parse::<u64>().is_ok())
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
    use std::time::Duration;

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
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(stderr, "psst: The operation is not supported.\n");
    }

    #[tokio::test]
    async fn deferred_commands_fail_without_touching_secrets_or_input() {
        let (code, stdout, stderr) = invoke(&[
            "psst", "--json", "message", "send", "--to", "worker", "--file", "-",
        ])
        .await;
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("unsupported"));

        for invalid in [
            vec!["psst", "--json", "message", "send", "--to", "worker"],
            vec!["psst", "--json", "squad", "join", "alpha", "--name", "one"],
            vec!["psst", "--json", "inbox", "--limit", "101"],
            vec!["psst", "--json", "listen", "--ack", "extra"],
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
