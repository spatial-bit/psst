use crate::{AppServerConfig, AppServerError, CodexAppServerHost};
use psst_application::{
    ActivationFuture, ActivationHost, ActivationPolicy, ActivationRuntime, ActivationSource,
    ConfigFlags, ConfigInputs, ConfigResolver, ObservationFailure, PlatformPaths, ProfileBinding,
    ProfilePaths, RuntimeSpec, SessionError, SessionRuntime, WakeMetadata, load_profile,
    verify_profile_origin,
};
use psst_client::{Client, ClientConfig, Error as ClientError};
use psst_protocol::{AgentModeDto, ClientMetadata, InboxResponse};
use std::{collections::BTreeMap, io, sync::Arc, time::Duration};
use tokio::sync::Mutex;

pub struct CodexActivation {
    activation: ActivationRuntime,
    host: Arc<CodexAppServerHost>,
    source: Arc<CyclingSessionSource>,
}

impl std::fmt::Debug for CodexActivation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexActivation")
            .finish_non_exhaustive()
    }
}

impl CodexActivation {
    /// Stops activation, reaps App Server and its MCP children, then releases any observer-owned
    /// cooperative profile session.
    ///
    /// # Errors
    /// Returns an error if either owned host cannot be shut down coherently.
    pub async fn shutdown(&self) -> Result<(), AppServerError> {
        self.activation.shutdown().await;
        self.host.shutdown().await?;
        self.source.shutdown().await
    }
}

/// Opens the configured, already-bound Psst profile and starts the Codex wake observer.
///
/// # Errors
/// Fails closed on invalid configuration, missing/stale authority, App Server incompatibility, or
/// activation-contract failure.
pub async fn start_from_environment() -> Result<CodexActivation, AppServerError> {
    let platform = PlatformPaths::detect().map_err(|_| AppServerError::Configuration)?;
    let environment = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect::<BTreeMap<_, _>>();
    let resolved = ConfigResolver::new(platform)
        .resolve(&ConfigInputs {
            flags: ConfigFlags::default(),
            environment: environment.clone(),
        })
        .map_err(|_| AppServerError::Configuration)?;
    let host = CodexAppServerHost::prepare(AppServerConfig::from_environment(
        resolved.relay_origin.value.clone(),
        resolved.profile.value.clone(),
        &environment,
    )?)
    .await?;
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
        .map_err(map_session_start)?;
        binding = load_profile(&paths.metadata).map_err(map_io)?;
    }
    let binding = binding.ok_or(AppServerError::Configuration)?;
    verify_profile_origin(&binding, &resolved.relay_origin.value)
        .map_err(|_| AppServerError::Configuration)?;
    let client = Arc::new(
        Client::new(&resolved.relay_origin.value, ClientConfig::default())
            .map_err(|_| AppServerError::Configuration)?,
    );
    let source = Arc::new(CyclingSessionSource {
        client,
        binding,
        paths,
        profile: resolved.profile.value,
        runtime: Mutex::new(None),
    });
    let activation = ActivationRuntime::start(
        Arc::clone(&source) as Arc<dyn ActivationSource>,
        Arc::clone(&host) as Arc<dyn ActivationHost>,
        ActivationPolicy::default(),
    )
    .map_err(|_| AppServerError::Configuration)?;
    Ok(CodexActivation {
        activation,
        host,
        source,
    })
}

struct CyclingSessionSource {
    client: Arc<Client>,
    binding: ProfileBinding,
    paths: ProfilePaths,
    profile: String,
    runtime: Mutex<Option<Arc<SessionRuntime>>>,
}

impl CyclingSessionSource {
    async fn shutdown(&self) -> Result<(), AppServerError> {
        let mut slot = self.runtime.lock().await;
        let Some(runtime) = slot.take() else {
            return Ok(());
        };
        runtime.shutdown().await.map_err(map_session_start)
    }

    async fn ensure_runtime(
        &self,
        slot: &mut Option<Arc<SessionRuntime>>,
    ) -> Result<Arc<SessionRuntime>, ObservationFailure> {
        if let Some(runtime) = slot.as_ref() {
            return Ok(Arc::clone(runtime));
        }
        let runtime = Arc::new(
            SessionRuntime::start(
                Arc::clone(&self.client),
                RuntimeSpec {
                    profile: self.binding.clone(),
                    paths: self.paths.clone(),
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
            .map_err(|error| classify_session(&error))?,
        );
        *slot = Some(Arc::clone(&runtime));
        Ok(runtime)
    }
}

impl ActivationSource for CyclingSessionSource {
    fn observe(
        &self,
        maximum_wait: Duration,
    ) -> ActivationFuture<'_, Result<Option<WakeMetadata>, ObservationFailure>> {
        Box::pin(async move {
            let mut slot = self.runtime.lock().await;
            let runtime = self.ensure_runtime(&mut slot).await?;
            let wait_seconds = u8::try_from(maximum_wait.as_secs().min(30))
                .expect("activation wait is bounded to 30 seconds");
            let inbox = runtime
                .inbox(100, wait_seconds)
                .await
                .map_err(|error| classify_session(&error))?;
            let wake = wake_from_inbox(&self.profile, &self.binding.squad_name, inbox)?;
            if wake.is_some() {
                runtime
                    .shutdown()
                    .await
                    .map_err(|_| ObservationFailure::Permanent)?;
                slot.take();
            }
            Ok(wake)
        })
    }
}

fn wake_from_inbox(
    profile: &str,
    squad: &str,
    response: InboxResponse,
) -> Result<Option<WakeMetadata>, ObservationFailure> {
    if response.pending_count == 0 {
        return Ok(None);
    }
    WakeMetadata::new(
        profile.to_owned(),
        squad.to_owned(),
        response.pending_count,
        response
            .highest_priority
            .ok_or(ObservationFailure::Permanent)?,
        response
            .oldest_message_id
            .ok_or(ObservationFailure::Permanent)?,
    )
    .map(Some)
    .map_err(|_| ObservationFailure::Permanent)
}

fn classify_session(error: &SessionError) -> ObservationFailure {
    match error {
        SessionError::Relay(
            ClientError::Transport(_)
            | ClientError::Timeout
            | ClientError::OutcomeUnknown
            | ClientError::ClientBusy
            | ClientError::RetryExhausted { .. }
            | ClientError::Api {
                retryable: true, ..
            },
        )
        | SessionError::NotReady
        | SessionError::OperationCapacity
        | SessionError::SendCapacity => ObservationFailure::Retryable,
        SessionError::Local(error)
            if matches!(
                error.kind(),
                io::ErrorKind::AddrInUse | io::ErrorKind::WouldBlock
            ) =>
        {
            ObservationFailure::Retryable
        }
        SessionError::Local(_)
        | SessionError::Relay(_)
        | SessionError::ShutdownTimedOut
        | SessionError::Unbound
        | SessionError::RecoveryOutcomeUnknown => ObservationFailure::Permanent,
    }
}

fn map_io(_: io::Error) -> AppServerError {
    AppServerError::Configuration
}

#[allow(clippy::needless_pass_by_value)]
fn map_session_start(error: SessionError) -> AppServerError {
    match classify_session(&error) {
        ObservationFailure::Retryable => AppServerError::Launch,
        ObservationFailure::Permanent => AppServerError::Configuration,
    }
}
