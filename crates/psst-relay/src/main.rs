use std::{net::SocketAddr, path::PathBuf, process::ExitCode};

use psst_relay::{LogFormat, RelayConfig, init_tracing, process_result_for_serve_error, serve};
use tokio::sync::watch;

fn configuration() -> Result<RelayConfig, Box<dyn std::error::Error + Send + Sync>> {
    let database =
        std::env::var_os("PSST_DATABASE").map_or_else(|| PathBuf::from("psst.db"), PathBuf::from);
    let mut config = RelayConfig::local(database);
    if let Ok(bind) = std::env::var("PSST_BIND") {
        config.bind = bind.parse::<SocketAddr>()?;
    }
    config.allow_lan = std::env::var("PSST_ALLOW_LAN").is_ok_and(|value| value == "1");
    config.log_level = std::env::var("PSST_LOG").unwrap_or_else(|_| "info".into());
    config.log_format = if std::env::var("PSST_LOG_FORMAT").is_ok_and(|value| value == "json") {
        LogFormat::Json
    } else {
        LogFormat::Text
    };
    config.validate()?;
    Ok(config)
}

#[tokio::main]
async fn main() -> ExitCode {
    let config = match configuration() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("psst-relay: invalid configuration: {error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = init_tracing(config.log_format, &config.log_level) {
        eprintln!("psst-relay: logging initialization failed: {error}");
        return ExitCode::from(2);
    }
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown_tx.send(true);
        }
    });
    match serve(config, shutdown_rx).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "relay stopped with an error");
            process_result_for_serve_error(error.as_ref())
        }
    }
}
