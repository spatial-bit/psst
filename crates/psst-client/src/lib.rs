//! Typed, bounded HTTP client for the Psst relay.

#![forbid(unsafe_code)]

use http::header::{AUTHORIZATION, CONTENT_TYPE};
use psst_protocol::{
    AckMessagesRequest, AckMessagesResponse, ArchiveSquadRequest, ArchiveSquadResponse,
    CreateSquadRequest, CreateSquadResponse, ErrorEnvelope, GetSquadResponse, HealthResponse,
    HeartbeatRequest, HeartbeatResponse, InboxQuery, InboxResponse, JSON_CONTENT_TYPE,
    JoinSquadRequest, JoinSquadResponse, LeaveSquadRequest, LeaveSquadResponse, ListSquadsResponse,
    MessagePriorityDto, MessageSequence, ReadyResponse, ResumeSquadRequest, RosterResponse,
    SESSION_CREDENTIAL_HEADER, SendMessageRequest, SendMessageResponse, SessionResponse,
    SquadSummary, TranscriptQuery, TranscriptResponse, Validate,
};
use reqwest::{Method, StatusCode, Url};
use serde::{Serialize, de::DeserializeOwned};
use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

mod credential_store;
pub use credential_store::{CredentialBinding, CredentialFault, CredentialStore};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Explicit network and retry bounds.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub long_poll_margin: Duration,
    pub retry: RetryPolicy,
    pub max_in_flight: usize,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(10),
            long_poll_margin: Duration::from_secs(5),
            retry: RetryPolicy::default(),
            max_in_flight: 32,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    /// Total attempts, including the first request.
    pub max_attempts: u8,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_millis(500),
        }
    }
}

/// In-memory authority. It cannot be displayed or serialized.
pub struct Credential {
    value: reqwest::header::HeaderValue,
    instance_id: String,
}

impl Credential {
    /// Returns the non-secret instance identity bound into this authority.
    #[must_use]
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
    fn from_response(value: &reqwest::header::HeaderValue) -> Result<Self, Error> {
        let raw = value.to_str().map_err(|_| Error::MalformedCredential)?;
        // Reuse the protocol's canonical parser without retaining another token copy.
        let parsed = psst_protocol::SessionCredential::parse_session_value(raw)
            .map_err(|_| Error::MalformedCredential)?;
        let instance_id = parsed.instance_id().to_string();
        let mut authorization = reqwest::header::HeaderValue::from_str(&format!("Bearer {raw}"))
            .map_err(|_| Error::MalformedCredential)?;
        authorization.set_sensitive(true);
        Ok(Self {
            value: authorization,
            instance_id,
        })
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Credential([REDACTED])")
    }
}

pub struct Session {
    pub response: JoinSquadResponse,
    pub credential: Credential,
}

impl fmt::Debug for Session {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Session")
            .field("response", &self.response)
            .field("credential", &self.credential)
            .finish()
    }
}

pub enum Error {
    InvalidBaseUrl,
    InvalidConfiguration,
    InvalidRequest,
    MalformedCredential,
    Transport(reqwest::Error),
    Timeout,
    OutcomeUnknown,
    Api {
        status: u16,
        code: psst_protocol::ApiErrorCode,
        retryable: bool,
    },
    MalformedResponse {
        status: u16,
    },
    ResponseTooLarge,
    UnexpectedHttp {
        status: u16,
    },
    ClientBusy,
    RetryExhausted {
        attempts: u8,
        last: Box<Error>,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBaseUrl => formatter.write_str("invalid relay base URL"),
            Self::InvalidConfiguration => formatter.write_str("invalid client configuration"),
            Self::InvalidRequest => formatter.write_str("invalid client request"),
            Self::MalformedCredential => {
                formatter.write_str("relay returned a malformed credential")
            }
            Self::Transport(_) => formatter.write_str("relay transport failed"),
            Self::Timeout => formatter.write_str("relay request timed out"),
            Self::OutcomeUnknown => formatter.write_str("session operation outcome is unknown"),
            Self::Api { status, code, .. } => {
                write!(formatter, "relay error {status} ({code:?})")
            }
            Self::MalformedResponse { status } => {
                write!(formatter, "relay returned a malformed response ({status})")
            }
            Self::ResponseTooLarge => {
                formatter.write_str("relay response exceeded the client limit")
            }
            Self::UnexpectedHttp { status } => {
                write!(formatter, "unexpected relay HTTP response ({status})")
            }
            Self::ClientBusy => formatter.write_str("client concurrency limit reached"),
            Self::RetryExhausted { attempts, .. } => {
                write!(
                    formatter,
                    "relay retry budget exhausted after {attempts} attempts"
                )
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[derive(Clone, Copy)]
enum RetryClass {
    Never,
    Safe,
    Deduplicated,
}

trait ResponseValidate {
    fn validate_response(&self) -> Result<(), Error>;
}

macro_rules! response_uses_protocol_validation {
    ($($ty:ty),+ $(,)?) => {
        $(impl ResponseValidate for $ty {
            fn validate_response(&self) -> Result<(), Error> {
                self.validate().map_err(|_| Error::MalformedResponse { status: 200 })
            }
        })+
    };
}

response_uses_protocol_validation!(
    Vec<SquadSummary>,
    SquadSummary,
    ArchiveSquadResponse,
    LeaveSquadResponse,
    RosterResponse,
    SendMessageResponse,
    AckMessagesResponse,
    TranscriptResponse,
);

impl ResponseValidate for SessionResponse {
    fn validate_response(&self) -> Result<(), Error> {
        use psst_core::{AgentId, InstanceId, MemberName, MembershipId, Role, SquadId, SquadName};
        let valid = AgentId::new(&self.agent_id).is_ok()
            && MembershipId::new(&self.membership_id).is_ok()
            && InstanceId::new(&self.instance_id).is_ok()
            && SquadId::new(&self.squad.id).is_ok()
            && SquadName::new(&self.squad.name).is_ok()
            && MemberName::new(&self.member_name).is_ok()
            && Role::new(&self.role).is_ok()
            && self.squad.validate().is_ok()
            && (1..=300).contains(&self.heartbeat_interval_seconds)
            && (2..=900).contains(&self.lease_seconds)
            && self.lease_seconds > self.heartbeat_interval_seconds;
        valid
            .then_some(())
            .ok_or(Error::MalformedResponse { status: 200 })
    }
}

impl ResponseValidate for HeartbeatResponse {
    fn validate_response(&self) -> Result<(), Error> {
        (1..=300)
            .contains(&self.heartbeat_interval_seconds)
            .then_some(())
            .ok_or(Error::MalformedResponse { status: 200 })
    }
}

impl ResponseValidate for HealthResponse {
    fn validate_response(&self) -> Result<(), Error> {
        (self.status == "ok")
            .then_some(())
            .ok_or(Error::MalformedResponse { status: 200 })
    }
}

impl ResponseValidate for ReadyResponse {
    fn validate_response(&self) -> Result<(), Error> {
        (self.status == "ready" && self.schema_version > 0)
            .then_some(())
            .ok_or(Error::MalformedResponse { status: 200 })
    }
}

impl ResponseValidate for InboxResponse {
    fn validate_response(&self) -> Result<(), Error> {
        self.validate()
            .map_err(|_| Error::MalformedResponse { status: 200 })
    }
}

pub struct Client {
    base: Url,
    http: reqwest::Client,
    config: ClientConfig,
    in_flight: Arc<Semaphore>,
    sleeper: Arc<dyn Sleeper>,
}

pub trait Sleeper: Send + Sync + 'static {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

struct TokioSleeper;
impl Sleeper for TokioSleeper {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Immutable send operation whose idempotency key survives any failed attempt.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct PreparedSendIdentity(Arc<str>);

impl fmt::Debug for PreparedSendIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedSendIdentity([OPAQUE])")
    }
}

#[derive(Clone)]
pub struct PreparedSend {
    request: Arc<SendMessageRequest>,
    identity: PreparedSendIdentity,
}

impl PreparedSend {
    #[must_use]
    pub fn request(&self) -> &SendMessageRequest {
        &self.request
    }

    /// Stable identity for this in-memory operation and its retry attempts.
    #[must_use]
    pub fn operation_identity(&self) -> PreparedSendIdentity {
        self.identity.clone()
    }

    /// Bytes retained while this operation is owned by a session supervisor.
    #[must_use]
    pub fn retained_body_bytes(&self) -> usize {
        self.request.body.len()
    }
}

impl fmt::Debug for PreparedSend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSend")
            .field("operation", &"[REDACTED]")
            .field("body", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

// Every operation returns the crate's closed `Error` taxonomy; repeating that same
// `# Errors` paragraph on each thin endpoint wrapper would obscure the API surface.
#[allow(clippy::missing_errors_doc)]
impl Client {
    /// Returns this client's canonical relay origin, always with a trailing slash.
    #[must_use]
    pub fn origin(&self) -> &str {
        self.base.as_str()
    }

    /// Creates a bounded client. Base URLs must be absolute HTTP(S) origins with no query,
    /// fragment, credentials, or non-root path.
    ///
    /// # Errors
    /// Returns a configuration or URL error when a bound or origin is invalid.
    pub fn new(base: &str, config: ClientConfig) -> Result<Self, Error> {
        Self::new_with_sleeper(base, config, Arc::new(TokioSleeper))
    }

    pub fn new_with_sleeper(
        base: &str,
        config: ClientConfig,
        sleeper: Arc<dyn Sleeper>,
    ) -> Result<Self, Error> {
        if config.connect_timeout.is_zero()
            || config.request_timeout.is_zero()
            || config.retry.max_attempts == 0
            || config.retry.max_attempts > 10
            || config.max_in_flight == 0
            || config.max_in_flight > 1024
            || config.retry.max_backoff < config.retry.initial_backoff
        {
            return Err(Error::InvalidConfiguration);
        }
        let mut base = Url::parse(base).map_err(|_| Error::InvalidBaseUrl)?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || !matches!(base.path(), "" | "/")
        {
            return Err(Error::InvalidBaseUrl);
        }
        base.set_path("/");
        let http = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .pool_max_idle_per_host(8)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(Error::Transport)?;
        let in_flight = Arc::new(Semaphore::new(config.max_in_flight));
        Ok(Self {
            base,
            http,
            config,
            in_flight,
            sleeper,
        })
    }

    pub async fn health(&self) -> Result<HealthResponse, Error> {
        self.execute(
            Method::GET,
            "healthz",
            None::<&()>,
            None,
            self.config.request_timeout,
            RetryClass::Safe,
        )
        .await
    }
    pub async fn ready(&self) -> Result<ReadyResponse, Error> {
        self.execute(
            Method::GET,
            "readyz",
            None::<&()>,
            None,
            self.config.request_timeout,
            RetryClass::Safe,
        )
        .await
    }
    pub async fn list_squads(&self) -> Result<ListSquadsResponse, Error> {
        self.execute(
            Method::GET,
            "v1/squads",
            None::<&()>,
            None,
            self.config.request_timeout,
            RetryClass::Safe,
        )
        .await
    }
    pub async fn create_squad(
        &self,
        request: &CreateSquadRequest,
    ) -> Result<CreateSquadResponse, Error> {
        request.validate().map_err(|_| Error::InvalidRequest)?;
        self.execute(
            Method::POST,
            "v1/squads",
            Some(request),
            None,
            self.config.request_timeout,
            RetryClass::Never,
        )
        .await
    }
    pub async fn describe_squad(&self, squad: &str) -> Result<GetSquadResponse, Error> {
        let path = squad_path(squad, "")?;
        self.execute(
            Method::GET,
            &path,
            None::<&()>,
            None,
            self.config.request_timeout,
            RetryClass::Safe,
        )
        .await
    }
    pub async fn join(&self, squad: &str, request: &JoinSquadRequest) -> Result<Session, Error> {
        request.validate().map_err(|_| Error::InvalidRequest)?;
        let path = squad_path(squad, "/join")?;
        let (response, credential) = self
            .execute_issued(Method::POST, &path, request, None)
            .await
            .map_err(|error| match error {
                Error::Transport(_) | Error::Timeout => Error::OutcomeUnknown,
                other => other,
            })?;
        Ok(Session {
            response,
            credential,
        })
    }
    pub async fn resume(
        &self,
        squad: &str,
        request: &ResumeSquadRequest,
        credential: &Credential,
    ) -> Result<Session, Error> {
        request.validate().map_err(|_| Error::InvalidRequest)?;
        let path = squad_path(squad, "/resume")?;
        let (response, credential) = self
            .execute_issued(Method::POST, &path, request, Some(credential))
            .await
            .map_err(|error| match error {
                Error::Transport(_) | Error::Timeout => Error::OutcomeUnknown,
                other => other,
            })?;
        Ok(Session {
            response,
            credential,
        })
    }
    pub async fn leave(
        &self,
        squad: &str,
        credential: &Credential,
    ) -> Result<LeaveSquadResponse, Error> {
        let path = squad_path(squad, "/leave")?;
        self.execute(
            Method::POST,
            &path,
            Some(&LeaveSquadRequest::default()),
            Some(credential),
            self.config.request_timeout,
            RetryClass::Never,
        )
        .await
        .map_err(|error| match error {
            Error::Transport(_) | Error::Timeout => Error::OutcomeUnknown,
            other => other,
        })
    }
    pub async fn archive_squad(
        &self,
        squad: &str,
        credential: &Credential,
    ) -> Result<ArchiveSquadResponse, Error> {
        let path = squad_path(squad, "/archive")?;
        self.execute(
            Method::POST,
            &path,
            Some(&ArchiveSquadRequest::default()),
            Some(credential),
            self.config.request_timeout,
            RetryClass::Never,
        )
        .await
    }
    pub async fn roster(
        &self,
        squad: &str,
        credential: &Credential,
    ) -> Result<RosterResponse, Error> {
        let path = squad_path(squad, "/roster")?;
        self.execute(
            Method::GET,
            &path,
            None::<&()>,
            Some(credential),
            self.config.request_timeout,
            RetryClass::Safe,
        )
        .await
    }
    pub async fn heartbeat(
        &self,
        request: &HeartbeatRequest,
        credential: &Credential,
    ) -> Result<HeartbeatResponse, Error> {
        request.validate().map_err(|_| Error::InvalidRequest)?;
        self.execute(
            Method::POST,
            "v1/heartbeat",
            Some(request),
            Some(credential),
            self.config.request_timeout,
            RetryClass::Never,
        )
        .await
    }
    pub async fn send(
        &self,
        recipient: String,
        body: String,
        priority: MessagePriorityDto,
        reply_to: Option<String>,
        correlation_id: Option<String>,
        credential: &Credential,
    ) -> Result<SendMessageResponse, Error> {
        let prepared = self.prepare_send(recipient, body, priority, reply_to, correlation_id)?;
        self.send_prepared(&prepared, credential).await
    }
    pub fn prepare_send(
        &self,
        recipient: String,
        body: String,
        priority: MessagePriorityDto,
        reply_to: Option<String>,
        correlation_id: Option<String>,
    ) -> Result<PreparedSend, Error> {
        let key = dedupe_key()?;
        let request = SendMessageRequest {
            recipient,
            body,
            priority,
            dedupe_key: key,
            reply_to,
            correlation_id,
        };
        request.validate().map_err(|_| Error::InvalidRequest)?;
        Ok(PreparedSend {
            identity: PreparedSendIdentity(Arc::from(request.dedupe_key.as_str())),
            request: Arc::new(request),
        })
    }
    pub async fn send_prepared(
        &self,
        prepared: &PreparedSend,
        credential: &Credential,
    ) -> Result<SendMessageResponse, Error> {
        self.send_with_request(&prepared.request, credential).await
    }
    /// Sends with a caller-supplied request, preserving its exact key across all attempts.
    pub async fn send_with_request(
        &self,
        request: &SendMessageRequest,
        credential: &Credential,
    ) -> Result<SendMessageResponse, Error> {
        request.validate().map_err(|_| Error::InvalidRequest)?;
        self.execute(
            Method::POST,
            "v1/messages",
            Some(request),
            Some(credential),
            self.config.request_timeout,
            RetryClass::Deduplicated,
        )
        .await
    }
    pub async fn inbox(
        &self,
        limit: u16,
        wait_seconds: u8,
        credential: &Credential,
    ) -> Result<InboxResponse, Error> {
        InboxQuery {
            limit,
            wait_seconds,
        }
        .validate()
        .map_err(|_| Error::InvalidRequest)?;
        let path = format!("v1/inbox?limit={limit}&wait={wait_seconds}");
        let timeout = Duration::from_secs(u64::from(wait_seconds))
            .saturating_add(self.config.long_poll_margin);
        self.execute(
            Method::GET,
            &path,
            None::<&()>,
            Some(credential),
            timeout,
            RetryClass::Safe,
        )
        .await
    }
    pub async fn acknowledge(
        &self,
        request: &AckMessagesRequest,
        credential: &Credential,
    ) -> Result<AckMessagesResponse, Error> {
        request.validate().map_err(|_| Error::InvalidRequest)?;
        self.execute(
            Method::POST,
            "v1/messages/ack",
            Some(request),
            Some(credential),
            self.config.request_timeout,
            RetryClass::Deduplicated,
        )
        .await
    }
    pub async fn transcript(
        &self,
        squad: &str,
        after: MessageSequence,
        limit: u16,
        credential: &Credential,
    ) -> Result<TranscriptResponse, Error> {
        TranscriptQuery { after, limit }
            .validate()
            .map_err(|_| Error::InvalidRequest)?;
        let base = squad_path(squad, "/transcript")?;
        self.execute(
            Method::GET,
            &format!("{base}?after={}&limit={limit}", after.value()),
            None::<&()>,
            Some(credential),
            self.config.request_timeout,
            RetryClass::Safe,
        )
        .await
    }

    async fn execute_issued<
        B: Serialize + ?Sized,
        R: DeserializeOwned + Serialize + ResponseValidate,
    >(
        &self,
        method: Method,
        path: &str,
        body: &B,
        credential: Option<&Credential>,
    ) -> Result<(R, Credential), Error> {
        tokio::time::timeout(
            self.config.request_timeout,
            self.execute_issued_inner(method, path, body, credential),
        )
        .await
        .map_err(|_| Error::Timeout)?
    }

    async fn execute_issued_inner<
        B: Serialize + ?Sized,
        R: DeserializeOwned + Serialize + ResponseValidate,
    >(
        &self,
        method: Method,
        path: &str,
        body: &B,
        credential: Option<&Credential>,
    ) -> Result<(R, Credential), Error> {
        let _permit = Arc::clone(&self.in_flight)
            .try_acquire_owned()
            .map_err(|_| Error::ClientBusy)?;
        let response = self
            .request(
                method,
                path,
                Some(body),
                credential,
                self.config.request_timeout,
            )
            .await?;
        if response.status() != StatusCode::OK {
            return match decode::<R>(response).await {
                Err(error) => Err(error),
                Ok(_) => Err(Error::UnexpectedHttp { status: 200 }),
            };
        }
        if response
            .headers()
            .get_all(SESSION_CREDENTIAL_HEADER)
            .iter()
            .count()
            != 1
            || !response
                .headers()
                .get_all(http::header::CACHE_CONTROL)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .flat_map(|value| value.split(','))
                .any(|directive| directive.trim().eq_ignore_ascii_case("no-store"))
        {
            return Err(Error::MalformedCredential);
        }
        let issued = response
            .headers()
            .get(SESSION_CREDENTIAL_HEADER)
            .ok_or(Error::MalformedCredential)?
            .clone();
        let value = decode(response).await?;
        let credential = Credential::from_response(&issued)?;
        let instance = serde_json::to_value(&value).ok().and_then(|json| {
            json.get("instance_id")
                .and_then(|field| field.as_str())
                .map(str::to_owned)
        });
        if instance.as_deref() != Some(credential.instance_id.as_str()) {
            return Err(Error::MalformedCredential);
        }
        Ok((value, credential))
    }

    async fn execute<B: Serialize + ?Sized, R: DeserializeOwned + ResponseValidate>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        credential: Option<&Credential>,
        timeout: Duration,
        class: RetryClass,
    ) -> Result<R, Error> {
        tokio::time::timeout(
            timeout,
            self.execute_inner(method, path, body, credential, timeout, class),
        )
        .await
        .map_err(|_| Error::Timeout)?
    }

    async fn execute_inner<B: Serialize + ?Sized, R: DeserializeOwned + ResponseValidate>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        credential: Option<&Credential>,
        timeout: Duration,
        class: RetryClass,
    ) -> Result<R, Error> {
        let attempts = match class {
            RetryClass::Never => 1,
            _ => self.config.retry.max_attempts,
        };
        let mut backoff = self.config.retry.initial_backoff;
        for attempt in 1..=attempts {
            let permit = Arc::clone(&self.in_flight)
                .try_acquire_owned()
                .map_err(|_| Error::ClientBusy)?;
            match self
                .request(method.clone(), path, body, credential, timeout)
                .await
            {
                Ok(response) => {
                    let status = response.status();
                    let result = decode(response).await;
                    let retryable_api = matches!(
                        &result,
                        Err(Error::Api {
                            retryable: true,
                            ..
                        })
                    );
                    let retryable_transport =
                        matches!(&result, Err(error) if transport_retryable(error));
                    let retryable = retryable_api || retryable_transport;
                    if retryable && attempt == attempts && attempts > 1 {
                        let Err(last) = result else {
                            unreachable!("retryable result is an error")
                        };
                        return Err(Error::RetryExhausted {
                            attempts,
                            last: Box::new(last),
                        });
                    }
                    if !retryable || attempt == attempts {
                        return result;
                    }
                    if retryable_api
                        && !matches!(
                            status,
                            StatusCode::TOO_MANY_REQUESTS
                                | StatusCode::SERVICE_UNAVAILABLE
                                | StatusCode::INTERNAL_SERVER_ERROR
                        )
                    {
                        return result;
                    }
                }
                Err(error) => {
                    if attempt == attempts && attempts > 1 && transport_retryable(&error) {
                        return Err(Error::RetryExhausted {
                            attempts,
                            last: Box::new(error),
                        });
                    }
                    if attempt == attempts || !transport_retryable(&error) {
                        return Err(error);
                    }
                }
            }
            drop(permit);
            self.sleeper.sleep(backoff).await;
            backoff = backoff.saturating_mul(2).min(self.config.retry.max_backoff);
        }
        unreachable!("attempt count is nonzero")
    }

    async fn request<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        credential: Option<&Credential>,
        timeout: Duration,
    ) -> Result<reqwest::Response, Error> {
        let url = self.base.join(path).map_err(|_| Error::InvalidBaseUrl)?;
        let mut request = self
            .http
            .request(method, url)
            .timeout(timeout)
            .header(CONTENT_TYPE, JSON_CONTENT_TYPE);
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(credential) = credential {
            request = request.header(AUTHORIZATION, credential.value.clone());
        }
        request.send().await.map_err(Error::Transport)
    }
}

fn transport_retryable(error: &Error) -> bool {
    matches!(error, Error::Timeout)
        || matches!(error, Error::Transport(error) if error.is_timeout() || error.is_connect() || error.is_request() || error.is_body() || error.is_decode())
}

async fn decode<R: DeserializeOwned + ResponseValidate>(
    mut response: reqwest::Response,
) -> Result<R, Error> {
    let status = response.status();
    if status == StatusCode::REQUEST_TIMEOUT {
        return Err(Error::Timeout);
    }
    let is_json = response
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case(JSON_CONTENT_TYPE));
    if !is_json {
        return Err(Error::UnexpectedHttp {
            status: status.as_u16(),
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(Error::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(Error::Transport)? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(Error::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    if status.is_success() {
        if status != StatusCode::OK {
            return Err(Error::UnexpectedHttp {
                status: status.as_u16(),
            });
        }
        let value: R = serde_json::from_slice(&bytes).map_err(|_| Error::MalformedResponse {
            status: status.as_u16(),
        })?;
        value.validate_response()?;
        Ok(value)
    } else {
        let envelope: ErrorEnvelope =
            serde_json::from_slice(&bytes).map_err(|_| Error::UnexpectedHttp {
                status: status.as_u16(),
            })?;
        envelope
            .error
            .validate()
            .map_err(|_| Error::UnexpectedHttp {
                status: status.as_u16(),
            })?;
        if envelope.error.code.http_status() != status.as_u16() {
            return Err(Error::UnexpectedHttp {
                status: status.as_u16(),
            });
        }
        Err(Error::Api {
            status: status.as_u16(),
            code: envelope.error.code,
            retryable: envelope.error.retryable,
        })
    }
}

fn dedupe_key() -> Result<String, Error> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|_| Error::InvalidConfiguration)?;
    let mut key = String::with_capacity(37);
    key.push_str("sdk-");
    for byte in random {
        use fmt::Write;
        write!(key, "{byte:02x}").expect("String writes do not fail");
    }
    Ok(key)
}

fn squad_path(squad: &str, suffix: &str) -> Result<String, Error> {
    psst_core::SquadName::new(squad).map_err(|_| Error::InvalidRequest)?;
    let mut encoded = String::new();
    for byte in squad.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use fmt::Write;
            write!(encoded, "%{byte:02X}").expect("String writes do not fail");
        }
    }
    Ok(format!("v1/squads/{encoded}{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::HeaderMap,
        response::IntoResponse,
        routing::{get, post},
    };
    use psst_protocol::{AgentModeDto, ApiTimestamp, ClientMetadata, SquadStateDto};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    struct RecordingSleeper {
        durations: Mutex<Vec<Duration>>,
        block: bool,
        entered: tokio::sync::Notify,
    }

    struct TestClock(AtomicI64);
    impl psst_relay::TimeSource for TestClock {
        fn now(&self) -> psst_core::UnixMillis {
            psst_core::UnixMillis::new(self.0.load(Ordering::SeqCst)).unwrap()
        }
    }
    impl Sleeper for RecordingSleeper {
        fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.durations.lock().unwrap().push(duration);
            self.entered.notify_one();
            Box::pin(async move {
                if self.block {
                    std::future::pending::<()>().await;
                }
            })
        }
    }

    async fn server(app: Router) -> String {
        server_with_handle(app).await.0
    }

    #[tokio::test]
    async fn invalid_send_requests_are_rejected_before_transport() {
        let client = client("http://127.0.0.1:1");
        for (recipient, body, reply_to, correlation_id) in [
            ("x".repeat(65), "ok".into(), None, None),
            (
                "worker".into(),
                "x".repeat(psst_core::MessageBody::MAX_BYTES + 1),
                None,
                None,
            ),
            (
                "worker".into(),
                "ok".into(),
                Some(format!("msg_{}", "a".repeat(125))),
                None,
            ),
            ("worker".into(), "ok".into(), None, Some("x".repeat(257))),
        ] {
            assert!(matches!(
                client.prepare_send(
                    recipient,
                    body,
                    MessagePriorityDto::Normal,
                    reply_to,
                    correlation_id,
                ),
                Err(Error::InvalidRequest)
            ));
        }
        let credential = Credential::from_response(&reqwest::header::HeaderValue::from_static(
            "ins_one.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ))
        .unwrap();
        let request = SendMessageRequest {
            recipient: "worker".into(),
            body: "ok".into(),
            priority: MessagePriorityDto::Normal,
            dedupe_key: "x".repeat(257),
            reply_to: None,
            correlation_id: None,
        };
        assert!(matches!(
            client.send_with_request(&request, &credential).await,
            Err(Error::InvalidRequest)
        ));
    }

    #[tokio::test]
    async fn every_public_request_family_rejects_invalid_values_before_transport() {
        let client = client("http://127.0.0.1:1");
        let credential = Credential::from_response(&reqwest::header::HeaderValue::from_static(
            "ins_one.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ))
        .unwrap();
        assert!(matches!(
            client
                .create_squad(&CreateSquadRequest {
                    name: "INVALID".into(),
                    mission: "mission".into(),
                })
                .await,
            Err(Error::InvalidRequest)
        ));
        let invalid_client = ClientMetadata {
            kind: String::new(),
            hostname: None,
            version: None,
        };
        assert!(matches!(
            client
                .join(
                    "alpha",
                    &JoinSquadRequest {
                        name: "worker".into(),
                        role: "worker".into(),
                        mode: AgentModeDto::Cooperative,
                        client: invalid_client.clone(),
                        mission: None,
                    },
                )
                .await,
            Err(Error::InvalidRequest)
        ));
        assert!(matches!(
            client
                .resume(
                    "alpha",
                    &ResumeSquadRequest {
                        mode: AgentModeDto::Cooperative,
                        client: invalid_client,
                    },
                    &credential,
                )
                .await,
            Err(Error::InvalidRequest)
        ));
        assert!(matches!(
            client
                .heartbeat(
                    &HeartbeatRequest {
                        availability: psst_protocol::AvailabilityDto::Unknown,
                        availability_source: psst_protocol::AvailabilitySourceDto::AgentReported,
                    },
                    &credential,
                )
                .await,
            Err(Error::InvalidRequest)
        ));
        assert!(matches!(
            client
                .acknowledge(
                    &AckMessagesRequest {
                        message_ids: vec![],
                    },
                    &credential,
                )
                .await,
            Err(Error::InvalidRequest)
        ));
        assert!(matches!(
            client.inbox(0, 0, &credential).await,
            Err(Error::InvalidRequest)
        ));
        assert!(matches!(
            client
                .transcript("alpha", MessageSequence::default(), 0, &credential)
                .await,
            Err(Error::InvalidRequest)
        ));
        assert!(matches!(
            client.describe_squad("INVALID").await,
            Err(Error::InvalidRequest)
        ));
        assert!(matches!(
            client.roster(&"x".repeat(65), &credential).await,
            Err(Error::InvalidRequest)
        ));
    }

    #[test]
    fn session_and_heartbeat_responses_enforce_semantic_identity_and_cadence() {
        let mut session: SessionResponse = serde_json::from_value(serde_json::json!({
            "agent_id": "agt_one",
            "membership_id": "mem_one",
            "instance_id": "ins_one",
            "squad": {
                "id": "sqd_one",
                "name": "alpha",
                "mission": "test",
                "state": "active",
                "created_at": "2026-01-01T00:00:00.000Z"
            },
            "member_name": "worker",
            "role": "worker",
            "heartbeat_interval_seconds": 10,
            "lease_seconds": 30,
            "lease_expires_at": "2026-01-01T00:00:30.000Z"
        }))
        .unwrap();
        assert!(session.validate_response().is_ok());
        session.agent_id = "agent-one".into();
        assert!(matches!(
            session.validate_response(),
            Err(Error::MalformedResponse { status: 200 })
        ));
        session.agent_id = "agt_one".into();
        session.heartbeat_interval_seconds = 301;
        assert!(session.validate_response().is_err());

        let heartbeat: HeartbeatResponse = serde_json::from_value(serde_json::json!({
            "lease_expires_at": "2026-01-01T00:00:30.000Z",
            "heartbeat_interval_seconds": 0
        }))
        .unwrap();
        assert!(heartbeat.validate_response().is_err());
    }

    async fn server_with_handle(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), handle)
    }

    async fn raw_server(responses: Vec<Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).await.unwrap();
                stream.write_all(&response).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        format!("http://{address}")
    }

    fn client(base: &str) -> Client {
        Client::new(base, ClientConfig::default()).unwrap()
    }

    #[test]
    fn base_url_and_configuration_are_bounded() {
        assert!(Client::new("ftp://host", ClientConfig::default()).is_err());
        assert!(Client::new("http://user:pass@host", ClientConfig::default()).is_err());
        assert!(Client::new("http://host/path", ClientConfig::default()).is_err());
        let config = ClientConfig {
            retry: RetryPolicy {
                max_attempts: 0,
                ..RetryPolicy::default()
            },
            ..ClientConfig::default()
        };
        assert!(Client::new("http://host", config).is_err());
        assert_eq!(
            Client::new("HTTP://Example.COM:80/", ClientConfig::default())
                .unwrap()
                .origin(),
            "http://example.com/"
        );
    }

    #[tokio::test]
    async fn issued_credential_is_applied_only_as_authorization_and_redacted() {
        const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let seen = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/v1/squads/s/join",
                post(|| async {
                    let body = session_response();
                    (
                        [
                            (SESSION_CREDENTIAL_HEADER, format!("ins_one.{TOKEN}")),
                            ("cache-control", "no-store".to_owned()),
                        ],
                        Json(body),
                    )
                }),
            )
            .route(
                "/v1/squads/s/roster",
                get({
                    let seen = Arc::clone(&seen);
                    move |headers: HeaderMap| async move {
                        *seen.lock().unwrap() = headers
                            .get(AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned);
                        Json(RosterResponse {
                            squad: "s".into(),
                            members: vec![],
                        })
                    }
                }),
            );
        let base = server(app).await;
        let session = client(&base)
            .join(
                "s",
                &JoinSquadRequest {
                    name: "a".into(),
                    role: "r".into(),
                    mode: AgentModeDto::Cooperative,
                    client: ClientMetadata {
                        kind: "test".into(),
                        hostname: None,
                        version: None,
                    },
                    mission: None,
                },
            )
            .await
            .unwrap();
        client(&base)
            .roster("s", &session.credential)
            .await
            .unwrap();
        let expected = format!("Bearer ins_one.{TOKEN}");
        assert_eq!(seen.lock().unwrap().as_deref(), Some(expected.as_str()));
        assert!(!format!("{session:?}").contains(TOKEN));
    }

    #[tokio::test]
    async fn retries_deduplicated_send_with_the_exact_same_key() {
        let keys = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(
                |State(keys): State<Arc<Mutex<Vec<String>>>>,
                 Json(request): Json<SendMessageRequest>| async move {
                    let mut keys = keys.lock().unwrap();
                    keys.push(request.dedupe_key);
                    if keys.len() == 1 {
                        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":{"code":"database_busy","message":"busy","retryable":true,"details":{}}}))).into_response();
                    }
                    (StatusCode::OK, Json(serde_json::json!({"message":{"sequence":1,"id":"msg_one","squad":"s","sender":"a","recipient":"b","body":"hi","priority":"normal","created_at":"2026-08-07T01:02:03.004Z"},"idempotent_replay":true}))).into_response()
                },
            ))
            .with_state(Arc::clone(&keys));
        let base = server(app).await;
        let credential = Credential::from_response(&reqwest::header::HeaderValue::from_static(
            "ins_one.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ))
        .unwrap();
        let result = client(&base)
            .send(
                "b".into(),
                "hi".into(),
                MessagePriorityDto::Normal,
                None,
                None,
                &credential,
            )
            .await
            .unwrap();
        assert!(result.idempotent_replay);
        let keys = keys.lock().unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], keys[1]);
    }

    #[tokio::test]
    async fn ambiguous_committed_send_retries_one_logical_message() {
        let directory = tempfile::tempdir().unwrap();
        let (worker, worker_join) =
            psst_relay::StoreWorker::start(&directory.path().join("ambiguous.db"), 32).unwrap();
        let (upstream, upstream_task) =
            server_with_handle(psst_relay::router(worker.clone())).await;
        let upstream_client = client(&upstream);
        upstream_client
            .create_squad(&CreateSquadRequest {
                name: "s".into(),
                mission: "proxy proof".into(),
            })
            .await
            .unwrap();
        let join = |name: &str| JoinSquadRequest {
            name: name.into(),
            role: "worker".into(),
            mode: AgentModeDto::Cooperative,
            client: ClientMetadata {
                kind: "test".into(),
                hostname: None,
                version: None,
            },
            mission: None,
        };
        let alice = upstream_client.join("s", &join("a")).await.unwrap();
        let _bob = upstream_client.join("s", &join("b")).await.unwrap();
        let keys = Arc::new(Mutex::new(Vec::<String>::new()));
        let app = Router::new()
            .route(
                "/v1/messages",
                post(
                    |State((upstream, keys)): State<(String, Arc<Mutex<Vec<String>>>)>,
                     headers: HeaderMap,
                     Json(request): Json<SendMessageRequest>| async move {
                        let attempt = {
                            let mut keys = keys.lock().unwrap();
                            keys.push(request.dedupe_key.clone());
                            keys.len()
                        };
                        let response = reqwest::Client::new()
                            .post(format!("{upstream}/v1/messages"))
                            .header(AUTHORIZATION, headers.get(AUTHORIZATION).unwrap().clone())
                            .json(&request)
                            .send()
                            .await
                            .unwrap();
                        assert_eq!(response.status(), StatusCode::OK);
                        let response: serde_json::Value = response.json().await.unwrap();
                        assert!(attempt != 1, "simulated proxy drop after durable commit");
                        Json(response)
                    },
                ),
            )
            .with_state((upstream.clone(), Arc::clone(&keys)));
        let base = server(app).await;
        let prepared = client(&base)
            .prepare_send(
                "b".into(),
                "hi".into(),
                MessagePriorityDto::Normal,
                None,
                None,
            )
            .unwrap();
        let response = client(&base)
            .send_prepared(&prepared, &alice.credential)
            .await
            .unwrap();
        assert!(response.idempotent_replay);
        let transcript = upstream_client
            .transcript("s", MessageSequence::default(), 10, &alice.credential)
            .await
            .unwrap();
        assert_eq!(transcript.messages.len(), 1);
        assert_eq!(transcript.messages[0].id, response.message.id);
        let keys = keys.lock().unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], keys[1]);
        drop(keys);
        worker.stop().unwrap();
        worker_join.join().unwrap().unwrap();
        upstream_task.abort();
    }

    #[tokio::test]
    async fn redirects_are_not_followed_and_lifecycle_timeout_is_unknown() {
        let redirect = Router::new()
            .route(
                "/healthz",
                get(|| async {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [(http::header::LOCATION, "/target")],
                    )
                }),
            )
            .route(
                "/target",
                get(|| async {
                    Json(HealthResponse {
                        status: "wrong".into(),
                    })
                }),
            );
        let base = server(redirect).await;
        assert!(matches!(
            client(&base).health().await,
            Err(Error::UnexpectedHttp { status: 307 })
        ));

        let slow = Router::new().route(
            "/v1/squads/s/join",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Json(session_response())
            }),
        );
        let base = server(slow).await;
        let config = ClientConfig {
            request_timeout: Duration::from_millis(20),
            retry: RetryPolicy {
                max_attempts: 1,
                ..RetryPolicy::default()
            },
            ..ClientConfig::default()
        };
        let request = JoinSquadRequest {
            name: "a".into(),
            role: "r".into(),
            mode: AgentModeDto::Cooperative,
            client: ClientMetadata {
                kind: "test".into(),
                hostname: None,
                version: None,
            },
            mission: None,
        };
        assert!(matches!(
            Client::new(&base, config)
                .unwrap()
                .join("s", &request)
                .await,
            Err(Error::OutcomeUnknown)
        ));
    }

    #[tokio::test]
    async fn issuance_requires_no_store_and_matching_instance() {
        let app = Router::new().route(
            "/v1/squads/s/join",
            post(|| async {
                (
                    [(
                        SESSION_CREDENTIAL_HEADER,
                        "ins_other.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                    )],
                    Json(session_response()),
                )
            }),
        );
        let base = server(app).await;
        let request = JoinSquadRequest {
            name: "a".into(),
            role: "r".into(),
            mode: AgentModeDto::Cooperative,
            client: ClientMetadata {
                kind: "test".into(),
                hostname: None,
                version: None,
            },
            mission: None,
        };
        assert!(matches!(
            client(&base).join("s", &request).await,
            Err(Error::MalformedCredential)
        ));
    }

    #[tokio::test]
    async fn retry_backoff_is_injectable_bounded_and_cancellable() {
        let busy = Router::new().route("/healthz", get(|| async {
            (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":{"code":"database_busy","message":"busy","retryable":true,"details":{}}})))
        }));
        let base = server(busy.clone()).await;
        let sleeper = Arc::new(RecordingSleeper {
            durations: Mutex::new(Vec::new()),
            block: false,
            entered: tokio::sync::Notify::new(),
        });
        let result = Client::new_with_sleeper(&base, ClientConfig::default(), sleeper.clone())
            .unwrap()
            .health()
            .await;
        assert!(matches!(
            result,
            Err(Error::RetryExhausted { attempts: 3, .. })
        ));
        assert_eq!(
            *sleeper.durations.lock().unwrap(),
            vec![Duration::from_millis(50), Duration::from_millis(100)]
        );

        let base = server(busy).await;
        let sleeper = Arc::new(RecordingSleeper {
            durations: Mutex::new(Vec::new()),
            block: true,
            entered: tokio::sync::Notify::new(),
        });
        let client = Arc::new(
            Client::new_with_sleeper(&base, ClientConfig::default(), sleeper.clone()).unwrap(),
        );
        let task = tokio::spawn({
            let client = Arc::clone(&client);
            async move { client.health().await }
        });
        sleeper.entered.notified().await;
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert_eq!(sleeper.durations.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn concurrency_limit_is_fail_fast() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let app = Router::new().route(
            "/healthz",
            get({
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                move || async move {
                    entered.notify_one();
                    release.notified().await;
                    Json(HealthResponse {
                        status: "ok".into(),
                    })
                }
            }),
        );
        let base = server(app).await;
        let config = ClientConfig {
            max_in_flight: 1,
            retry: RetryPolicy {
                max_attempts: 1,
                ..RetryPolicy::default()
            },
            ..ClientConfig::default()
        };
        let client = Arc::new(Client::new(&base, config).unwrap());
        let first = tokio::spawn({
            let client = Arc::clone(&client);
            async move { client.health().await }
        });
        entered.notified().await;
        assert!(matches!(client.health().await, Err(Error::ClientBusy)));
        release.notify_waiters();
        first.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn body_truncation_retries_but_oversized_chunked_response_does_not() {
        let truncated = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{\"status\":\"".to_vec();
        let valid_body = br#"{"status":"ok"}"#;
        let valid = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", valid_body.len()).into_bytes().into_iter().chain(valid_body.iter().copied()).collect();
        let base = raw_server(vec![truncated, valid]).await;
        assert_eq!(client(&base).health().await.unwrap().status, "ok");

        let body = vec![b'x'; MAX_RESPONSE_BYTES + 1];
        let mut oversized = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:X}\r\n", body.len()).into_bytes();
        oversized.extend(body);
        oversized.extend_from_slice(b"\r\n0\r\n\r\n");
        let base = raw_server(vec![oversized]).await;
        let config = ClientConfig {
            retry: RetryPolicy {
                max_attempts: 1,
                ..RetryPolicy::default()
            },
            ..ClientConfig::default()
        };
        assert!(matches!(
            Client::new(&base, config).unwrap().health().await,
            Err(Error::ResponseTooLarge)
        ));
    }

    #[tokio::test]
    async fn protocol_semantics_status_and_diagnostics_are_bounded_and_secret_safe() {
        const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let status_mismatch = Router::new().route("/healthz", get(|| async {
            (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":{"code":"internal_error","message":"server echoed AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","retryable":true,"details":{"echo":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}}})))
        }));
        let base = server(status_mismatch).await;
        let error = client(&base).health().await.unwrap_err();
        assert!(matches!(error, Error::UnexpectedHttp { status: 503 }));
        assert!(!format!("{error}").contains(TOKEN));
        assert!(!format!("{error:?}").contains(TOKEN));
        assert!(std::error::Error::source(&error).is_none());

        let malformed = Router::new()
            .route("/healthz", get(|| async { Json(HealthResponse { status: "future".into() }) }))
            .route("/readyz", get(|| async { (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":{"code":"database_busy","message":"","retryable":true,"details":{}}}))) }));
        let base = server(malformed).await;
        assert!(matches!(
            client(&base).health().await,
            Err(Error::MalformedResponse { status: 200 })
        ));
        assert!(matches!(
            client(&base).ready().await,
            Err(Error::UnexpectedHttp { status: 503 })
        ));

        let message = serde_json::json!({"sequence":1,"id":"msg_one","squad":"s","sender":"a","recipient":"b","body":"x","priority":"normal","created_at":"2026-08-07T01:02:03.004Z"});
        let messages = vec![message; 101];
        let app = Router::new().route(
            "/v1/inbox",
            get(move || {
                let messages = messages.clone();
                async move { Json(serde_json::json!({"messages":messages,"pending_count":101})) }
            }),
        );
        let base = server(app).await;
        let credential = Credential::from_response(&reqwest::header::HeaderValue::from_static(
            "ins_one.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ))
        .unwrap();
        assert!(matches!(
            client(&base).inbox(100, 0, &credential).await,
            Err(Error::MalformedResponse { status: 200 })
        ));

        let app = Router::new().route(
            "/v1/messages",
            post(|| async {
                Json(serde_json::json!({
                    "message": {
                        "sequence": 1,
                        "id": "not-a-message-id",
                        "squad": "sqd_one",
                        "sender": "mem_one",
                        "recipient": "mem_two",
                        "body": "x",
                        "priority": "normal",
                        "created_at": "2026-08-07T01:02:03.004Z"
                    },
                    "idempotent_replay": false
                }))
            }),
        );
        let base = server(app).await;
        let request = SendMessageRequest {
            recipient: "worker".into(),
            body: "hello".into(),
            priority: MessagePriorityDto::Normal,
            dedupe_key: "client-test-key".into(),
            reply_to: None,
            correlation_id: None,
        };
        assert!(matches!(
            client(&base).send_with_request(&request, &credential).await,
            Err(Error::MalformedResponse { status: 200 })
        ));
    }

    #[tokio::test]
    async fn total_operation_deadline_includes_response_and_backoff() {
        let slow = Router::new().route(
            "/healthz",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Json(HealthResponse {
                    status: "ok".into(),
                })
            }),
        );
        let base = server(slow).await;
        let config = ClientConfig {
            request_timeout: Duration::from_millis(25),
            ..ClientConfig::default()
        };
        let started = tokio::time::Instant::now();
        assert!(matches!(
            Client::new(&base, config).unwrap().health().await,
            Err(Error::Timeout)
        ));
        assert!(started.elapsed() < Duration::from_millis(250));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn complete_lifecycle_runs_against_the_real_relay() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("client-integration.db");
        let clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
        let (worker, worker_join) = psst_relay::StoreWorker::start_with_time(
            &database,
            32,
            Duration::from_secs(5),
            clock.clone(),
        )
        .unwrap();
        let (base, server_task) = server_with_handle(psst_relay::router(worker.clone())).await;
        let client = Client::new(&base, ClientConfig::default()).unwrap();
        assert_eq!(client.health().await.unwrap().status, "ok");
        assert_eq!(
            client.ready().await.unwrap().schema_version,
            psst_store::current_schema_version()
        );
        client
            .create_squad(&CreateSquadRequest {
                name: "team".into(),
                mission: "verify client".into(),
            })
            .await
            .unwrap();
        let join = |name: &str| JoinSquadRequest {
            name: name.into(),
            role: "worker".into(),
            mode: AgentModeDto::Cooperative,
            client: ClientMetadata {
                kind: "integration".into(),
                hostname: None,
                version: None,
            },
            mission: None,
        };
        let alice = client.join("team", &join("alice")).await.unwrap();
        let bob = client.join("team", &join("bob")).await.unwrap();
        assert_eq!(
            client
                .roster("team", &alice.credential)
                .await
                .unwrap()
                .members
                .len(),
            2
        );
        let sent = client
            .send(
                "bob".into(),
                "hello".into(),
                MessagePriorityDto::High,
                None,
                Some("thread-one".into()),
                &alice.credential,
            )
            .await
            .unwrap();

        clock.0.fetch_add(31_000, Ordering::SeqCst);

        worker.stop().unwrap();
        worker_join.join().unwrap().unwrap();
        server_task.abort();
        let (worker, worker_join) =
            psst_relay::StoreWorker::start_with_time(&database, 32, Duration::from_secs(5), clock)
                .unwrap();
        let (base, server_task) = server_with_handle(psst_relay::router(worker.clone())).await;
        let restarted = Client::new(&base, ClientConfig::default()).unwrap();
        let resume = ResumeSquadRequest {
            mode: AgentModeDto::Cooperative,
            client: ClientMetadata {
                kind: "integration".into(),
                hostname: None,
                version: None,
            },
        };
        let alice = restarted
            .resume("team", &resume, &alice.credential)
            .await
            .unwrap();
        let bob = restarted
            .resume("team", &resume, &bob.credential)
            .await
            .unwrap();
        let inbox = restarted.inbox(10, 0, &bob.credential).await.unwrap();
        assert_eq!(inbox.messages.len(), 1);
        assert_eq!(inbox.messages[0].id, sent.message.id);
        restarted
            .acknowledge(
                &AckMessagesRequest {
                    message_ids: vec![sent.message.id.clone()],
                },
                &bob.credential,
            )
            .await
            .unwrap();
        let transcript = restarted
            .transcript("team", MessageSequence::default(), 10, &alice.credential)
            .await
            .unwrap();
        assert_eq!(transcript.messages.len(), 1);
        restarted.leave("team", &bob.credential).await.unwrap();
        worker.stop().unwrap();
        worker_join.join().unwrap().unwrap();
        server_task.abort();
    }

    fn session_response() -> JoinSquadResponse {
        let timestamp: ApiTimestamp = serde_json::from_str("\"2026-08-07T01:02:03.004Z\"").unwrap();
        JoinSquadResponse {
            agent_id: "agt_one".into(),
            membership_id: "mem_one".into(),
            instance_id: "ins_one".into(),
            squad: psst_protocol::SquadSummary {
                id: "sqd_one".into(),
                name: "s".into(),
                mission: "m".into(),
                state: SquadStateDto::Active,
                created_at: timestamp,
                archived_at: None,
            },
            member_name: "a".into(),
            role: "r".into(),
            heartbeat_interval_seconds: 10,
            lease_seconds: 30,
            lease_expires_at: timestamp,
        }
    }
}
