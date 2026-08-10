use crate::{AppServerConfig, AppServerError, CodexAppServerHost};
use psst_application::{
    ActivationHost, ActivationPolicy, ActivationRuntime, ActivationSource, ConfigFlags,
    ConfigInputs, ConfigResolver, PlatformPaths, ProfilePaths, RuntimeSpec,
    SessionActivationSource, SessionError, SessionRuntime, load_profile, verify_profile_origin,
};
use psst_client::{Client, ClientConfig};
use psst_protocol::{AgentModeDto, ClientMetadata};
use std::{collections::BTreeMap, io, sync::Arc, time::Duration};

pub struct CodexActivation {
    activation: ActivationRuntime,
    host: Arc<CodexAppServerHost>,
    runtime: Arc<SessionRuntime>,
}

impl std::fmt::Debug for CodexActivation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexActivation")
            .finish_non_exhaustive()
    }
}

impl CodexActivation {
    /// Stops observation and the App Server before releasing the cooperative profile session.
    ///
    /// # Errors
    /// Returns an error if either owned host cannot be shut down coherently.
    pub async fn shutdown(&self) -> Result<(), AppServerError> {
        self.activation.shutdown().await;
        self.host.shutdown().await?;
        self.runtime
            .shutdown()
            .await
            .map_err(|_| AppServerError::Exited)
    }
}

/// Opens the configured, already-bound Psst profile and starts the Codex wake observer.
///
/// # Errors
/// Fails closed on invalid configuration, missing/stale authority, App Server incompatibility, or
/// activation-contract failure.
pub async fn start_from_environment() -> Result<CodexActivation, AppServerError> {
    let host = CodexAppServerHost::connect(AppServerConfig::from_environment()?).await?;
    let platform = PlatformPaths::detect().map_err(|_| AppServerError::Configuration)?;
    let environment = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect::<BTreeMap<_, _>>();
    let resolved = ConfigResolver::new(platform)
        .resolve(&ConfigInputs {
            flags: ConfigFlags::default(),
            environment,
        })
        .map_err(|_| AppServerError::Configuration)?;
    let paths = ProfilePaths::for_profile(
        &resolved.paths,
        &resolved.relay_origin.value,
        &resolved.profile.value,
    )
    .map_err(|_| AppServerError::Configuration)?;
    let mut binding = load_profile(&paths.metadata).map_err(map_io)?;
    if binding.is_none() {
        SessionRuntime::recover_orphaned_leave(
            paths.clone(),
            resolved.relay_origin.value.clone(),
            resolved.profile.value.clone(),
        )
        .await
        .map_err(map_session)?;
        binding = load_profile(&paths.metadata).map_err(map_io)?;
    }
    let binding = binding.ok_or(AppServerError::Configuration)?;
    verify_profile_origin(&binding, &resolved.relay_origin.value)
        .map_err(|_| AppServerError::Configuration)?;
    let squad = binding.squad_name.clone();
    let client = Arc::new(
        Client::new(&resolved.relay_origin.value, ClientConfig::default())
            .map_err(|_| AppServerError::Configuration)?,
    );
    let runtime = Arc::new(
        SessionRuntime::start(
            client,
            RuntimeSpec {
                profile: binding,
                paths,
                mode: AgentModeDto::Harnessed,
                client_metadata: ClientMetadata {
                    kind: "psst-codex".into(),
                    hostname: None,
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                },
                shutdown_bound: Duration::from_secs(5),
            },
        )
        .await
        .map_err(map_session)?,
    );
    let source: Arc<dyn ActivationSource> = Arc::new(
        SessionActivationSource::new(Arc::clone(&runtime), resolved.profile.value, squad)
            .map_err(|_| AppServerError::Configuration)?,
    );
    let activation = ActivationRuntime::start(
        source,
        Arc::clone(&host) as Arc<dyn ActivationHost>,
        ActivationPolicy::default(),
    )
    .map_err(|_| AppServerError::Configuration)?;
    Ok(CodexActivation {
        activation,
        host,
        runtime,
    })
}

fn map_io(_: io::Error) -> AppServerError {
    AppServerError::Configuration
}

#[allow(clippy::needless_pass_by_value)]
fn map_session(error: SessionError) -> AppServerError {
    match error {
        SessionError::Relay(_) | SessionError::NotReady | SessionError::OperationCapacity => {
            AppServerError::Launch
        }
        SessionError::Local(_)
        | SessionError::ShutdownTimedOut
        | SessionError::Unbound
        | SessionError::RecoveryOutcomeUnknown
        | SessionError::SendCapacity => AppServerError::Configuration,
    }
}
