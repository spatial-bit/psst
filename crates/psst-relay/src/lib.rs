//! Bounded relay runtime. Product routes are intentionally added in later work units.

use std::future::IntoFuture;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, SyncSender, TrySendError},
};
use std::thread::JoinHandle;
use std::time::Duration;
use std::{
    pin::Pin,
    task::{Context, Poll},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use psst_core::{
    AgentMode, Availability, AvailabilitySource, InstanceId, MessageId, ResumeToken, SquadId,
    UnixMillis,
};
use psst_store::{
    AuthenticatedSession, InstanceRecord, JoinAndClaim, JoinAndClaimOutcome, JoinMembership,
    LeasePolicy, MessageRecord, RepositoryError, SendMessage, TranscriptQuery,
};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot, watch};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

pub const DEFAULT_QUEUE_CAPACITY: usize = 256;
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Debug)]
pub struct RelayConfig {
    pub bind: SocketAddr,
    pub allow_lan: bool,
    pub database: PathBuf,
    pub queue_capacity: usize,
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub max_body_bytes: usize,
    pub max_connections: usize,
    pub max_in_flight_requests: usize,
    pub log_format: LogFormat,
    pub log_level: String,
}

impl RelayConfig {
    #[must_use]
    pub fn local(database: impl Into<PathBuf>) -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7341),
            allow_lan: false,
            database: database.into(),
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            request_timeout: Duration::from_secs(5),
            shutdown_timeout: Duration::from_secs(10),
            max_body_bytes: 128 * 1024,
            max_connections: 128,
            max_in_flight_requests: 128,
            log_format: LogFormat::Text,
            log_level: "info".into(),
        }
    }

    /// Validates all safety and resource bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when LAN opt-in is absent or a bound is unsupported.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.bind.ip().is_loopback() && !self.allow_lan {
            return Err(ConfigError::LanRequiresOptIn);
        }
        if self.queue_capacity == 0
            || self.queue_capacity > 4096
            || self.request_timeout.is_zero()
            || self.shutdown_timeout.is_zero()
            || self.max_body_bytes == 0
            || self.max_body_bytes > 1024 * 1024
            || self.max_connections == 0
            || self.max_connections > 10_000
            || self.max_in_flight_requests == 0
            || self.max_in_flight_requests > 10_000
        {
            return Err(ConfigError::OutOfBounds);
        }
        EnvFilter::try_new(&self.log_level).map_err(|_| ConfigError::InvalidLogLevel)?;
        Ok(())
    }

    #[must_use]
    pub fn trusted_lan_warning(&self) -> Option<&'static str> {
        (!self.bind.ip().is_loopback()).then_some("WARNING: Psst has no TLS and trusts every process on this LAN; do not expose it to hostile networks or the internet.")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigError {
    LanRequiresOptIn,
    OutOfBounds,
    InvalidLogLevel,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::LanRequiresOptIn => "LAN binding requires explicit opt-in",
            Self::OutOfBounds => "relay configuration is outside supported bounds",
            Self::InvalidLogLevel => "relay log level/filter is invalid",
        })
    }
}
impl std::error::Error for ConfigError {}

/// Installs the relay process's text or JSON tracing subscriber.
///
/// # Errors
/// Returns an error for an invalid filter or an already-installed global subscriber.
pub fn init_tracing(
    format: LogFormat,
    level: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let filter = EnvFilter::try_new(level)?;
    match format {
        LogFormat::Text => tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .try_init()?,
        LogFormat::Json => tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().json())
            .try_init()?,
    }
    Ok(())
}

#[derive(Debug)]
pub struct ShutdownTimedOut;

impl std::fmt::Display for ShutdownTimedOut {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("relay shutdown deadline exceeded; checkpoint completion is unknown")
    }
}

impl std::error::Error for ShutdownTimedOut {}

/// Converts a serving failure to a process result. A forced-shutdown timeout
/// exits immediately because waiting for runtime teardown could wait forever
/// on the very thread whose deadline was exceeded.
#[must_use]
pub fn process_result_for_serve_error(
    error: &(dyn std::error::Error + 'static),
) -> std::process::ExitCode {
    if error.downcast_ref::<ShutdownTimedOut>().is_some() {
        std::process::exit(3);
    }
    std::process::ExitCode::from(1)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerError {
    RateLimited,
    Unavailable,
    Store,
    Timeout,
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::RateLimited => "store command queue is full",
            Self::Unavailable => "store worker is unavailable",
            Self::Store => "store operation failed",
            Self::Timeout => "store operation timed out",
        })
    }
}

impl std::error::Error for WorkerError {}

enum StoreCommand {
    #[cfg(test)]
    Block(Duration, oneshot::Sender<Result<(), WorkerError>>),
    #[cfg(test)]
    ControlledBlock {
        started: oneshot::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    Join(
        WorkerJoin,
        oneshot::Sender<Result<JoinAndClaimOutcome, RepositoryError>>,
    ),
    Heartbeat(
        Credential,
        Availability,
        AvailabilitySource,
        LeasePolicy,
        oneshot::Sender<Result<InstanceRecord, RepositoryError>>,
    ),
    Send(
        Credential,
        SendMessage,
        oneshot::Sender<Result<MessageRecord, RepositoryError>>,
    ),
    Pending(
        Credential,
        usize,
        oneshot::Sender<Result<Vec<MessageRecord>, RepositoryError>>,
    ),
    Acknowledge(
        Credential,
        Vec<MessageId>,
        oneshot::Sender<Result<(), RepositoryError>>,
    ),
    Transcript(
        Credential,
        TranscriptQuery,
        oneshot::Sender<Result<Vec<MessageRecord>, RepositoryError>>,
    ),
    Leave(Credential, oneshot::Sender<Result<(), RepositoryError>>),
    Archive(
        Credential,
        SquadId,
        oneshot::Sender<Result<(), RepositoryError>>,
    ),
    Resume(
        WorkerResume,
        oneshot::Sender<Result<InstanceRecord, RepositoryError>>,
    ),
    Ready(oneshot::Sender<Result<(), WorkerError>>),
    Checkpoint(oneshot::Sender<Result<(), WorkerError>>),
}

impl StoreCommand {
    fn cancel(self) {
        match self {
            #[cfg(test)]
            Self::Block(_, reply) => {
                let _ = reply.send(Err(WorkerError::Unavailable));
            }
            #[cfg(test)]
            Self::ControlledBlock { reply, .. } => {
                let _ = reply.send(Err(WorkerError::Unavailable));
            }
            Self::Ready(reply) | Self::Checkpoint(reply) => {
                let _ = reply.send(Err(WorkerError::Unavailable));
            }
            Self::Join(_, reply) => drop(reply),
            Self::Heartbeat(_, _, _, _, reply) => drop(reply),
            Self::Resume(_, reply) => drop(reply),
            Self::Send(_, _, reply) => drop(reply),
            Self::Pending(_, _, reply) => drop(reply),
            Self::Transcript(_, _, reply) => drop(reply),
            Self::Acknowledge(_, _, reply) => drop(reply),
            Self::Leave(_, reply) => drop(reply),
            Self::Archive(_, _, reply) => drop(reply),
        }
    }
}

enum SendFailure {
    Full,
    Disconnected,
}

/// Owned adapter credential used only across the in-process worker boundary.
pub struct Credential {
    pub instance_id: InstanceId,
    pub resume_token: ResumeToken,
}

pub trait TimeSource: Send + Sync + 'static {
    fn now(&self) -> UnixMillis;
}
pub struct SystemTimeSource;
impl TimeSource for SystemTimeSource {
    fn now(&self) -> UnixMillis {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time must be after Unix epoch")
            .as_millis();
        UnixMillis::new(i64::try_from(millis).expect("current epoch milliseconds fit i64"))
            .expect("current time is nonnegative")
    }
}

pub struct WorkerJoin {
    pub membership: JoinMembership,
    pub instance_id: InstanceId,
    pub mode: AgentMode,
    pub client_kind: String,
    pub hostname: Option<String>,
    pub availability: Availability,
    pub availability_source: AvailabilitySource,
    pub lease_policy: LeasePolicy,
}

pub struct WorkerResume {
    pub prior: Credential,
    pub new_instance: InstanceId,
    pub mode: AgentMode,
    pub client_kind: String,
    pub hostname: Option<String>,
    pub availability: Availability,
    pub availability_source: AvailabilitySource,
    pub lease_policy: LeasePolicy,
}

#[derive(Clone)]
pub struct StoreWorker {
    inner: Arc<WorkerInner>,
}

struct WorkerInner {
    sender: Mutex<Option<SyncSender<StoreCommand>>>,
    accepting: AtomicBool,
    request_timeout: Duration,
    shutdown: Arc<AtomicBool>,
}

#[allow(clippy::missing_errors_doc)]
impl StoreWorker {
    pub fn start(
        path: &Path,
        capacity: usize,
    ) -> Result<(Self, JoinHandle<Result<(), WorkerError>>), psst_store::StoreError> {
        Self::start_with_time(
            path,
            capacity,
            Duration::from_secs(5),
            Arc::new(SystemTimeSource),
        )
    }

    #[allow(clippy::too_many_lines)]
    pub fn start_with_time(
        path: &Path,
        capacity: usize,
        request_timeout: Duration,
        clock: Arc<dyn TimeSource>,
    ) -> Result<(Self, JoinHandle<Result<(), WorkerError>>), psst_store::StoreError> {
        Self::start_internal(path, capacity, request_timeout, clock, false)
    }

    #[cfg(test)]
    fn start_with_failed_shutdown_checkpoint(
        path: &Path,
    ) -> Result<(Self, JoinHandle<Result<(), WorkerError>>), psst_store::StoreError> {
        Self::start_internal(
            path,
            4,
            Duration::from_secs(1),
            Arc::new(SystemTimeSource),
            true,
        )
    }

    #[allow(clippy::too_many_lines)]
    fn start_internal(
        path: &Path,
        capacity: usize,
        request_timeout: Duration,
        clock: Arc<dyn TimeSource>,
        fail_shutdown_checkpoint: bool,
    ) -> Result<(Self, JoinHandle<Result<(), WorkerError>>), psst_store::StoreError> {
        let store = psst_store::Store::open(path)?;
        let (sender, receiver) = mpsc::sync_channel::<StoreCommand>(capacity);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let handle = std::thread::Builder::new()
            .name("psst-sqlite".into())
            .spawn(move || {
                let mut store = store;
                while let Ok(command) = receiver.recv() {
                    if worker_shutdown.load(Ordering::Acquire) {
                        command.cancel();
                        for queued in receiver.try_iter() {
                            queued.cancel();
                        }
                        break;
                    }
                    match command {
                        #[cfg(test)]
                        StoreCommand::Block(duration, reply) => {
                            std::thread::sleep(duration);
                            let _ = reply.send(Ok(()));
                        }
                        #[cfg(test)]
                        StoreCommand::ControlledBlock {
                            started,
                            release,
                            reply,
                        } => {
                            let _ = started.send(());
                            let result = release.recv().map_err(|_| WorkerError::Unavailable);
                            let _ = reply.send(result);
                        }
                        StoreCommand::Join(request, reply) => {
                            let now = clock.now();
                            let mut membership = request.membership;
                            membership.joined_at = now;
                            let result = store.join_and_claim(&JoinAndClaim {
                                membership,
                                instance_id: request.instance_id,
                                mode: request.mode,
                                client_kind: &request.client_kind,
                                hostname: request.hostname.as_deref(),
                                availability: request.availability,
                                availability_source: request.availability_source,
                                lease_policy: request.lease_policy,
                            });
                            let _ = reply.send(result);
                        }
                        StoreCommand::Heartbeat(
                            credential,
                            availability,
                            source,
                            policy,
                            reply,
                        ) => {
                            let session = session(&credential, clock.now());
                            let _ = reply.send(store.authenticated_heartbeat(
                                &session,
                                availability,
                                source,
                                policy,
                            ));
                        }
                        StoreCommand::Send(credential, mut request, reply) => {
                            let now = clock.now();
                            request.created_at = now;
                            let session = session(&credential, now);
                            let _ = reply.send(store.authenticated_send(&session, &request));
                        }
                        StoreCommand::Pending(credential, limit, reply) => {
                            let session = session(&credential, clock.now());
                            let _ = reply.send(store.authenticated_pending(&session, limit));
                        }
                        StoreCommand::Acknowledge(credential, ids, reply) => {
                            let session = session(&credential, clock.now());
                            let _ = reply.send(store.authenticated_acknowledge(&session, ids));
                        }
                        StoreCommand::Transcript(credential, query, reply) => {
                            let session = session(&credential, clock.now());
                            let _ = reply.send(store.authenticated_transcript(&session, &query));
                        }
                        StoreCommand::Leave(credential, reply) => {
                            let session = session(&credential, clock.now());
                            let _ = reply.send(store.authenticated_leave(&session));
                        }
                        StoreCommand::Archive(credential, squad, reply) => {
                            let session = session(&credential, clock.now());
                            let _ = reply.send(store.authenticated_archive(&session, &squad));
                        }
                        StoreCommand::Resume(request, reply) => {
                            let _ = reply.send(store.authenticated_resume(
                                &request.prior.instance_id,
                                &request.prior.resume_token,
                                request.new_instance,
                                request.mode,
                                &request.client_kind,
                                request.hostname.as_deref(),
                                request.availability,
                                request.availability_source,
                                clock.now(),
                                request.lease_policy,
                            ));
                        }
                        StoreCommand::Ready(reply) => {
                            let _ = reply.send(store.readiness().map_err(|_| WorkerError::Store));
                        }
                        StoreCommand::Checkpoint(reply) => {
                            let _ = reply.send(store.checkpoint().map_err(|_| WorkerError::Store));
                        }
                    }
                }
                if fail_shutdown_checkpoint {
                    Err(WorkerError::Store)
                } else {
                    store.checkpoint().map_err(|_| WorkerError::Store)
                }
            })
            .map_err(|_| psst_store::StoreError::WorkerUnavailable)?;
        Ok((
            Self {
                inner: Arc::new(WorkerInner {
                    sender: Mutex::new(Some(sender)),
                    accepting: AtomicBool::new(true),
                    request_timeout,
                    shutdown,
                }),
            },
            handle,
        ))
    }

    pub async fn ready(&self) -> Result<(), WorkerError> {
        self.request(StoreCommand::Ready).await
    }
    pub async fn checkpoint(&self) -> Result<(), WorkerError> {
        self.request(StoreCommand::Checkpoint).await
    }
    pub fn stop(&self) -> Result<(), WorkerError> {
        self.begin_shutdown();
        Ok(())
    }

    pub fn begin_shutdown(&self) {
        self.inner.accepting.store(false, Ordering::Release);
        self.inner.shutdown.store(true, Ordering::Release);
        self.inner
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    #[cfg(test)]
    async fn block_for(&self, duration: Duration) -> Result<(), WorkerError> {
        self.request(|reply| StoreCommand::Block(duration, reply))
            .await
    }

    #[cfg(test)]
    async fn controlled_block(
        &self,
        started: oneshot::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> Result<(), WorkerError> {
        self.request(|reply| StoreCommand::ControlledBlock {
            started,
            release,
            reply,
        })
        .await
    }

    pub async fn join(&self, request: WorkerJoin) -> Result<JoinAndClaimOutcome, DispatchError> {
        self.dispatch(|reply| StoreCommand::Join(request, reply))
            .await
    }
    pub async fn heartbeat(
        &self,
        credential: Credential,
        availability: Availability,
        source: AvailabilitySource,
        policy: LeasePolicy,
    ) -> Result<InstanceRecord, DispatchError> {
        self.dispatch(|reply| {
            StoreCommand::Heartbeat(credential, availability, source, policy, reply)
        })
        .await
    }
    pub async fn send(
        &self,
        credential: Credential,
        request: SendMessage,
    ) -> Result<MessageRecord, DispatchError> {
        self.dispatch(|reply| StoreCommand::Send(credential, request, reply))
            .await
    }
    pub async fn pending(
        &self,
        credential: Credential,
        limit: usize,
    ) -> Result<Vec<MessageRecord>, DispatchError> {
        self.dispatch(|reply| StoreCommand::Pending(credential, limit, reply))
            .await
    }
    pub async fn acknowledge(
        &self,
        credential: Credential,
        ids: Vec<MessageId>,
    ) -> Result<(), DispatchError> {
        self.dispatch(|reply| StoreCommand::Acknowledge(credential, ids, reply))
            .await
    }
    pub async fn transcript(
        &self,
        credential: Credential,
        query: TranscriptQuery,
    ) -> Result<Vec<MessageRecord>, DispatchError> {
        self.dispatch(|reply| StoreCommand::Transcript(credential, query, reply))
            .await
    }
    pub async fn leave(&self, credential: Credential) -> Result<(), DispatchError> {
        self.dispatch(|reply| StoreCommand::Leave(credential, reply))
            .await
    }
    pub async fn archive(
        &self,
        credential: Credential,
        squad: SquadId,
    ) -> Result<(), DispatchError> {
        self.dispatch(|reply| StoreCommand::Archive(credential, squad, reply))
            .await
    }
    pub async fn resume(&self, request: WorkerResume) -> Result<InstanceRecord, DispatchError> {
        self.dispatch(|reply| StoreCommand::Resume(request, reply))
            .await
    }

    async fn request(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<(), WorkerError>>) -> StoreCommand,
    ) -> Result<(), WorkerError> {
        let (reply, receive) = oneshot::channel();
        self.try_send(make(reply)).map_err(|error| match error {
            SendFailure::Full => WorkerError::RateLimited,
            SendFailure::Disconnected => WorkerError::Unavailable,
        })?;
        tokio::time::timeout(self.inner.request_timeout, receive)
            .await
            .map_err(|_| WorkerError::Timeout)?
            .map_err(|_| WorkerError::Unavailable)?
    }

    async fn dispatch<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, RepositoryError>>) -> StoreCommand,
    ) -> Result<T, DispatchError> {
        let (reply, receive) = oneshot::channel();
        self.try_send(make(reply)).map_err(|error| match error {
            SendFailure::Full => DispatchError::RateLimited,
            SendFailure::Disconnected => DispatchError::Unavailable,
        })?;
        tokio::time::timeout(self.inner.request_timeout, receive)
            .await
            .map_err(|_| DispatchError::Timeout)?
            .map_err(|_| DispatchError::Unavailable)?
            .map_err(DispatchError::Store)
    }

    fn try_send(&self, command: StoreCommand) -> Result<(), SendFailure> {
        if !self.inner.accepting.load(Ordering::Acquire) {
            return Err(SendFailure::Disconnected);
        }
        let guard = self
            .inner
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.as_ref() {
            Some(sender) => sender.try_send(command).map_err(|error| match error {
                TrySendError::Full(_) => SendFailure::Full,
                TrySendError::Disconnected(_) => SendFailure::Disconnected,
            }),
            None => Err(SendFailure::Disconnected),
        }
    }
}

fn session(credential: &Credential, now: UnixMillis) -> AuthenticatedSession<'_> {
    AuthenticatedSession {
        instance_id: &credential.instance_id,
        resume_token: &credential.resume_token,
        now,
    }
}

#[derive(Debug)]
pub enum DispatchError {
    RateLimited,
    Unavailable,
    Timeout,
    Store(RepositoryError),
}
impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RateLimited => "store command queue is full",
            Self::Unavailable => "store worker is unavailable",
            Self::Timeout => "store command timed out; committed completion is ambiguous",
            Self::Store(_) => "store command failed",
        })
    }
}
impl std::error::Error for DispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct AppState {
    worker: StoreWorker,
}

#[derive(Serialize)]
struct StatusBody {
    status: &'static str,
}

pub fn router(worker: StoreWorker) -> Router {
    router_with_limits(worker, 128 * 1024, 128, Duration::from_secs(5))
}

pub fn router_with_limits(
    worker: StoreWorker,
    max_body: usize,
    max_in_flight: usize,
    timeout: Duration,
) -> Router {
    apply_limits(
        Router::new()
            .route("/healthz", get(health))
            .route("/readyz", get(ready))
            .with_state(AppState { worker }),
        max_body,
        max_in_flight,
        timeout,
    )
}

fn apply_limits(
    router: Router,
    max_body: usize,
    max_in_flight: usize,
    timeout: Duration,
) -> Router {
    let admission = Arc::new(Semaphore::new(max_in_flight));
    router
        .layer(DefaultBodyLimit::max(max_body))
        .layer(middleware::from_fn(move |request, next| {
            admit_request(request, next, Arc::clone(&admission))
        }))
        .layer(middleware::from_fn(move |request, next| {
            request_deadline(request, next, timeout)
        }))
        .layer(middleware::from_fn(request_trace))
}

async fn admit_request(request: Request<Body>, next: Next, admission: Arc<Semaphore>) -> Response {
    let Ok(_permit) = admission.acquire_owned().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    next.run(request).await
}

/// Listener wrapper that owns one semaphore permit for the full lifetime of
/// every accepted connection. This bounds sockets, independently of the
/// request concurrency middleware (important for HTTP keep-alive).
pub struct LimitedTcpListener {
    inner: tokio::net::TcpListener,
    permits: Arc<Semaphore>,
}

impl LimitedTcpListener {
    #[must_use]
    pub fn new(inner: tokio::net::TcpListener, maximum: usize) -> Self {
        Self {
            inner,
            permits: Arc::new(Semaphore::new(maximum)),
        }
    }
}

pub struct LimitedTcpStream {
    inner: tokio::net::TcpStream,
    _permit: OwnedSemaphorePermit,
}

impl AsyncRead for LimitedTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}
impl AsyncWrite for LimitedTcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buffer)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl axum::serve::Listener for LimitedTcpListener {
    type Io = LimitedTcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let permit = Arc::clone(&self.permits)
                .acquire_owned()
                .await
                .expect("connection semaphore is never closed");
            match self.inner.accept().await {
                Ok((inner, address)) => {
                    return (
                        LimitedTcpStream {
                            inner,
                            _permit: permit,
                        },
                        address,
                    );
                }
                Err(error) => {
                    tracing::error!(%error, "listener accept failed");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

async fn request_trace(request: Request<Body>, next: Next) -> Response {
    let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let method = request.method().clone();
    let matched_route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or("unmatched", axum::extract::MatchedPath::as_str)
        .to_owned();
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    tracing::info!(
        request_id,
        method = %method,
        matched_route,
        status = response.status().as_u16(),
        latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "request completed"
    );
    response
}

async fn request_deadline(request: Request<Body>, next: Next, timeout: Duration) -> Response {
    tokio::time::timeout(timeout, next.run(request))
        .await
        .unwrap_or_else(|_| StatusCode::REQUEST_TIMEOUT.into_response())
}

async fn health() -> Json<StatusBody> {
    Json(StatusBody { status: "ok" })
}
async fn ready(State(state): State<AppState>) -> (StatusCode, Json<StatusBody>) {
    match state.worker.ready().await {
        Ok(()) => (StatusCode::OK, Json(StatusBody { status: "ready" })),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(StatusBody {
                status: "unavailable",
            }),
        ),
    }
}

/// Runs until shutdown, then drains HTTP, checkpoints the store, and joins its worker.
///
/// # Errors
///
/// Returns configuration, bind, store, serving, or bounded-shutdown failures.
pub async fn serve(
    config: RelayConfig,
    shutdown: watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_with_router_factory(config, shutdown, |worker, config| {
        router_with_limits(
            worker,
            config.max_body_bytes,
            config.max_in_flight_requests,
            config.request_timeout,
        )
    })
    .await
}

async fn serve_with_router_factory(
    config: RelayConfig,
    mut shutdown: watch::Receiver<bool>,
    make_router: impl FnOnce(StoreWorker, &RelayConfig) -> Router,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    config.validate()?;
    warn_if_lan(&config);
    let (worker, handle) = StoreWorker::start_with_time(
        &config.database,
        config.queue_capacity,
        config.request_timeout,
        Arc::new(SystemTimeSource),
    )?;
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    let bound = listener.local_addr()?;
    tracing::info!(bind = %bound, "relay started");
    let shutdown_started = Arc::new(tokio::sync::Notify::new());
    let shutdown_notice = Arc::clone(&shutdown_started);
    let server = axum::serve(
        LimitedTcpListener::new(listener, config.max_connections),
        make_router(worker.clone(), &config),
    )
    .with_graceful_shutdown(async move {
        while !*shutdown.borrow() && shutdown.changed().await.is_ok() {}
        shutdown_notice.notify_one();
    })
    .into_future();
    tokio::pin!(server);
    let already_done = tokio::select! {
        result = &mut server => Some(result),
        () = shutdown_started.notified() => None,
    };
    let deadline = tokio::time::Instant::now() + config.shutdown_timeout;
    let server_result = if let Some(result) = already_done {
        result
    } else {
        // Stop new database admission at the same instant HTTP draining
        // starts. The worker completes at most its current operation,
        // rejects queued work, then checkpoints.
        worker.begin_shutdown();
        if let Ok(result) = tokio::time::timeout_at(deadline, &mut server).await {
            result
        } else {
            return Err(Box::new(ShutdownTimedOut));
        }
    };
    worker.begin_shutdown();
    join_worker_until(handle, deadline).await?;
    server_result.map_err(Into::into)
}

fn warn_if_lan(config: &RelayConfig) {
    if let Some(warning) = config.trusted_lan_warning() {
        tracing::warn!(target: "psst::security", "{warning}");
    }
}

async fn join_worker_until(
    handle: JoinHandle<Result<(), WorkerError>>,
    deadline: tokio::time::Instant,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let join = tokio::task::spawn_blocking(move || handle.join());
    let joined = tokio::time::timeout_at(deadline, join)
        .await
        .map_err(|_| Box::new(ShutdownTimedOut) as Box<dyn std::error::Error + Send + Sync>)??;
    Ok(joined.map_err(|_| "store worker panicked")??)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        routing::{get, post},
    };
    use psst_core::{AgentId, MemberName, Mission, Role, SquadName};
    use tower::ServiceExt;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct LogCapture(Arc<Mutex<Vec<u8>>>);
    struct LogWriter(Arc<Mutex<Vec<u8>>>);
    impl<'a> MakeWriter<'a> for LogCapture {
        type Writer = LogWriter;
        fn make_writer(&'a self) -> Self::Writer {
            LogWriter(Arc::clone(&self.0))
        }
    }
    impl std::io::Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl LogCapture {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    struct FakeTime(UnixMillis);
    impl TimeSource for FakeTime {
        fn now(&self) -> UnixMillis {
            self.0
        }
    }

    #[test]
    fn loopback_is_default_and_lan_needs_opt_in() {
        let mut config = RelayConfig::local("test.db");
        assert!(config.bind.ip().is_loopback());
        config.bind = "0.0.0.0:7341".parse().unwrap();
        assert_eq!(config.validate(), Err(ConfigError::LanRequiresOptIn));
        config.allow_lan = true;
        assert!(config.validate().is_ok());
        assert!(config.trusted_lan_warning().is_some());
    }

    #[tokio::test]
    async fn health_is_independent_and_readiness_uses_store() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start(&directory.path().join("psst.db"), 4).unwrap();
        let app = router(worker.clone());
        let health = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        let ready = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ready.status(), StatusCode::OK);
        worker.stop().unwrap();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn timeout_cancels_wait_while_admitted_work_drains_and_queue_recovers() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start_with_time(
            &directory.path().join("psst.db"),
            1,
            Duration::from_millis(20),
            Arc::new(SystemTimeSource),
        )
        .unwrap();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = worker.clone();
        let block_task =
            tokio::spawn(async move { blocker.controlled_block(started_tx, release_rx).await });
        started_rx.await.unwrap();

        let (checkpoint_reply_tx, mut checkpoint_reply_rx) = oneshot::channel();
        assert!(
            worker
                .try_send(StoreCommand::Checkpoint(checkpoint_reply_tx))
                .is_ok()
        );
        assert_eq!(worker.ready().await, Err(WorkerError::RateLimited));
        assert_eq!(block_task.await.unwrap(), Err(WorkerError::Timeout));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut checkpoint_reply_rx)
                .await
                .is_err()
        );

        release_tx.send(()).unwrap();
        assert_eq!(checkpoint_reply_rx.await.unwrap(), Ok(()));
        assert_eq!(worker.ready().await, Ok(()));
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn worker_time_overrides_caller_supplied_join_timestamp() {
        let directory = tempfile::TempDir::new().unwrap();
        let authoritative = UnixMillis::new(9_999).unwrap();
        let (worker, handle) = StoreWorker::start_with_time(
            &directory.path().join("psst.db"),
            4,
            Duration::from_secs(1),
            Arc::new(FakeTime(authoritative)),
        )
        .unwrap();
        let outcome = worker
            .join(WorkerJoin {
                membership: JoinMembership {
                    squad_name: SquadName::new("alpha").unwrap(),
                    mission_if_missing: Some(Mission::new("clock proof").unwrap()),
                    squad_id_if_missing: SquadId::new("sqd_alpha").unwrap(),
                    agent_id: AgentId::new("agt_alice").unwrap(),
                    membership_id: psst_core::MembershipId::new("mem_alice").unwrap(),
                    member_name: MemberName::new("alice").unwrap(),
                    role: Role::new("tester").unwrap(),
                    joined_at: UnixMillis::new(1).unwrap(),
                },
                instance_id: InstanceId::new("ins_alice").unwrap(),
                mode: AgentMode::Cooperative,
                client_kind: "test".into(),
                hostname: None,
                availability: Availability::Unknown,
                availability_source: AvailabilitySource::Unknown,
                lease_policy: LeasePolicy::default(),
            })
            .await
            .unwrap();
        assert_eq!(outcome.membership.joined_at, authoritative);
        assert_eq!(outcome.claim().instance().created_at, authoritative);
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn forced_worker_shutdown_returns_within_deadline() {
        let handle = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            Ok(())
        });
        let started = tokio::time::Instant::now();
        let error = join_worker_until(handle, started + Duration::from_millis(20))
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<ShutdownTimedOut>().is_some());
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn normal_worker_completion_propagates_checkpoint_failure() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) =
            StoreWorker::start_with_failed_shutdown_checkpoint(&directory.path().join("psst.db"))
                .unwrap();
        worker.begin_shutdown();
        let error = join_worker_until(handle, tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<WorkerError>(),
            Some(&WorkerError::Store)
        );
    }

    #[test]
    fn hard_timeout_exit_bypasses_a_never_finishing_thread() {
        const CHILD: &str = "PSST_TEST_HARD_TIMEOUT_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let _wedged = std::thread::spawn(|| {
                loop {
                    std::thread::park();
                }
            });
            let _ = process_result_for_serve_error(&ShutdownTimedOut);
            unreachable!();
        }
        let started = std::time::Instant::now();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::hard_timeout_exit_bypasses_a_never_finishing_thread",
            ])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(3));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn shutdown_finishes_current_operation_cancels_queue_and_checkpoints() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start_with_time(
            &directory.path().join("psst.db"),
            4,
            Duration::from_secs(1),
            Arc::new(SystemTimeSource),
        )
        .unwrap();
        let active_worker = worker.clone();
        let active =
            tokio::spawn(async move { active_worker.block_for(Duration::from_millis(50)).await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        let queued_worker = worker.clone();
        let queued = tokio::spawn(async move { queued_worker.ready().await });
        tokio::time::sleep(Duration::from_millis(5)).await;
        worker.begin_shutdown();

        assert_eq!(active.await.unwrap(), Ok(()));
        assert_eq!(queued.await.unwrap(), Err(WorkerError::Unavailable));
        assert_eq!(worker.ready().await, Err(WorkerError::Unavailable));
        assert_eq!(handle.join().unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn body_limit_and_request_timeout_are_enforced() {
        async fn echo(body: String) -> String {
            body
        }
        async fn slow() -> &'static str {
            tokio::time::sleep(Duration::from_millis(50)).await;
            "late"
        }
        let app = Router::new()
            .route("/echo", post(echo))
            .route("/slow", get(slow))
            .layer(middleware::from_fn(|request, next| {
                request_deadline(request, next, Duration::from_millis(10))
            }))
            .layer(DefaultBodyLimit::max(4));
        let oversized = app
            .clone()
            .oneshot(
                Request::post("/echo")
                    .header("content-type", "text/plain")
                    .body(Body::from("12345"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let timed_out = app
            .oneshot(Request::get("/slow").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(timed_out.status(), StatusCode::REQUEST_TIMEOUT);
    }

    #[tokio::test]
    async fn in_flight_request_limit_is_independent_and_enforced() {
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let maximum = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handler = {
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            move || {
                let active = Arc::clone(&active);
                let maximum = Arc::clone(&maximum);
                async move {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(current, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    "ok"
                }
            }
        };
        let admission = Arc::new(Semaphore::new(1));
        let app = Router::new()
            .route("/work", get(handler))
            .layer(middleware::from_fn(move |request, next| {
                admit_request(request, next, Arc::clone(&admission))
            }));
        let first = tokio::spawn(
            app.clone()
                .oneshot(Request::get("/work").body(Body::empty()).unwrap()),
        );
        let second = tokio::spawn(app.oneshot(Request::get("/work").body(Body::empty()).unwrap()));
        assert_eq!(first.await.unwrap().unwrap().status(), StatusCode::OK);
        assert_eq!(second.await.unwrap().unwrap().status(), StatusCode::OK);
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn connection_limit_blocks_accept_until_owned_io_is_dropped() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut limited = LimitedTcpListener::new(listener, 1);
        let first_client = tokio::net::TcpStream::connect(address);
        let (first_client, (first_server, _)) =
            tokio::join!(first_client, axum::serve::Listener::accept(&mut limited));
        let _first_client = first_client.unwrap();

        let second_client = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut second_accept = Box::pin(axum::serve::Listener::accept(&mut limited));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut second_accept)
                .await
                .is_err()
        );
        drop(first_server);
        let (_second_server, _) = tokio::time::timeout(Duration::from_secs(1), second_accept)
            .await
            .unwrap();
        drop(second_client);
    }

    #[tokio::test]
    async fn readiness_is_unavailable_when_worker_is_closed() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start(&directory.path().join("psst.db"), 1).unwrap();
        let app = router(worker.clone());
        worker.begin_shutdown();
        let response = app
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert!(std::str::from_utf8(&body).unwrap().contains("unavailable"));
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn readiness_is_unavailable_while_store_queue_is_saturated() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start_with_time(
            &directory.path().join("psst.db"),
            1,
            Duration::from_secs(1),
            Arc::new(SystemTimeSource),
        )
        .unwrap();
        let blocker = worker.clone();
        let active =
            tokio::spawn(async move { blocker.block_for(Duration::from_millis(100)).await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        let queued_worker = worker.clone();
        let queued = tokio::spawn(async move { queued_worker.checkpoint().await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        let response = router(worker.clone())
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        active.await.unwrap().unwrap();
        queued.await.unwrap().unwrap();
        assert!(worker.ready().await.is_ok());
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn future_schema_and_wrong_application_database_fail_before_listener_startup() {
        let directory = tempfile::TempDir::new().unwrap();
        let future = directory.path().join("future.db");
        {
            let (worker, handle) = StoreWorker::start(&future, 1).unwrap();
            worker.begin_shutdown();
            handle.join().unwrap().unwrap();
            let connection = rusqlite::Connection::open(&future).unwrap();
            connection.execute("INSERT INTO schema_migrations(version, applied_at, checksum) VALUES (999, unixepoch(), 'future')", []).unwrap();
        }
        assert!(matches!(
            StoreWorker::start(&future, 1),
            Err(psst_store::StoreError::FutureSchema { .. })
        ));

        let wrong = directory.path().join("wrong.db");
        {
            let connection = rusqlite::Connection::open(&wrong).unwrap();
            connection
                .pragma_update(None, "application_id", 1234_i64)
                .unwrap();
        }
        assert!(matches!(
            StoreWorker::start(&wrong, 1),
            Err(psst_store::StoreError::UnexpectedApplicationId { actual: 1234 })
        ));
    }

    #[tokio::test]
    async fn wal_readiness_stays_ready_during_an_external_writer_lock() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let (worker, handle) = StoreWorker::start(&path, 2).unwrap();
        let writer = rusqlite::Connection::open(&path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert_eq!(worker.ready().await, Ok(()));
        writer.execute_batch("ROLLBACK").unwrap();
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    async fn raw_get(address: SocketAddr, path: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    #[tokio::test]
    async fn bound_server_is_healthy_ready_and_refuses_after_clean_shutdown() {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let directory = tempfile::TempDir::new().unwrap();
        let mut config = RelayConfig::local(directory.path().join("psst.db"));
        config.bind = address;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(serve(config, shutdown_rx));
        let mut connected = false;
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                connected = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(connected, "relay did not bind in time");
        assert!(
            raw_get(address, "/healthz")
                .await
                .starts_with("HTTP/1.1 200")
        );
        assert!(
            raw_get(address, "/readyz")
                .await
                .starts_with("HTTP/1.1 200")
        );
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }

    #[tokio::test]
    async fn real_serve_drains_admitted_request_and_refuses_new_connections() {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let directory = tempfile::TempDir::new().unwrap();
        let mut config = RelayConfig::local(directory.path().join("psst.db"));
        config.bind = address;
        config.shutdown_timeout = Duration::from_secs(5);
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let route_started = Arc::clone(&started);
        let route_release = Arc::clone(&release);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(serve_with_router_factory(
            config,
            shutdown_rx,
            move |worker, config| {
                let slow = move || {
                    let started = Arc::clone(&route_started);
                    let release = Arc::clone(&route_release);
                    async move {
                        started.notify_one();
                        release.notified().await;
                        "drained"
                    }
                };
                apply_limits(
                    Router::new()
                        .route("/healthz", get(health))
                        .route("/readyz", get(ready))
                        .route("/slow", get(slow))
                        .with_state(AppState { worker }),
                    config.max_body_bytes,
                    config.max_in_flight_requests,
                    config.request_timeout,
                )
            },
        ));
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let request = tokio::spawn(raw_get(address, "/slow"));
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .unwrap();
        shutdown_tx.send(true).unwrap();
        let shutdown_started_at = tokio::time::Instant::now();
        let mut refused = false;
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(address).await.is_err() {
                refused = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            refused,
            "listener continued accepting after shutdown signal"
        );
        release.notify_one();
        assert!(request.await.unwrap().starts_with("HTTP/1.1 200"));
        task.await.unwrap().unwrap();
        assert!(shutdown_started_at.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn request_logs_include_only_safe_metadata_and_lan_warning_is_visible() {
        let capture = LogCapture::default();
        let json_capture = LogCapture::default();
        let subscriber = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(capture.clone())
                    .without_time()
                    .with_ansi(false),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(json_capture.clone())
                    .without_time(),
            );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        runtime.block_on(async {
            let directory = tempfile::TempDir::new().unwrap();
            let (worker, handle) =
                StoreWorker::start(&directory.path().join("psst.db"), 2).unwrap();
            let secret = "resume-token-never-log";
            let body_secret = "body-secret-never-log";
            let response = router(worker.clone())
                .oneshot(
                    Request::get(format!("/healthz?token={secret}"))
                        .header("authorization", format!("Bearer {secret}"))
                        .body(Body::from(body_secret))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let mut lan = RelayConfig::local("unused.db");
            lan.bind = "0.0.0.0:7341".parse().unwrap();
            lan.allow_lan = true;
            warn_if_lan(&lan);
            worker.begin_shutdown();
            handle.join().unwrap().unwrap();
        });
        let logs = capture.text();
        assert!(logs.contains("request_id"));
        assert!(logs.contains("method=GET"), "{logs}");
        assert!(logs.contains("matched_route=\"/healthz\""), "{logs}");
        assert!(logs.contains("status=200"));
        assert!(logs.contains("no TLS"));
        assert!(!logs.contains("resume-token-never-log"));
        assert!(!logs.contains("body-secret-never-log"));
        assert!(!logs.to_ascii_lowercase().contains("authorization"));
        let json_logs = json_capture.text();
        let events: Vec<serde_json::Value> = json_logs
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let request = events
            .iter()
            .find(|event| event["fields"]["matched_route"] == "/healthz")
            .unwrap();
        assert_eq!(request["fields"]["method"], "GET");
        assert_eq!(request["fields"]["matched_route"], "/healthz");
        assert_eq!(request["fields"]["status"], 200);
        assert!(request["fields"]["latency_ms"].is_number());
        assert!(!json_logs.contains("resume-token-never-log"));
        assert!(!json_logs.contains("body-secret-never-log"));
        assert!(!json_logs.to_ascii_lowercase().contains("authorization"));
    }
}
