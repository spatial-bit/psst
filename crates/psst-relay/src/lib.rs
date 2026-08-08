//! Bounded relay runtime. Product routes are intentionally added in later work units.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
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
    extract::{DefaultBodyLimit, FromRequest, FromRequestParts, Path as AxumPath, Query, State},
    http::{HeaderMap, Request, StatusCode, header::AUTHORIZATION, request::Parts},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use psst_core::{
    AgentId, AgentMode, Availability, AvailabilitySource, CorrelationId, DedupeKey, InstanceId,
    MemberName, MembershipId, MessageBody, MessageId, MessagePriority, Mission, ResumeToken, Role,
    SquadId, SquadName, SquadState, UnixMillis,
};
use psst_protocol::{
    AckMessagesRequest, AckMessagesResponse, AgentModeDto, ApiErrorCode, ApiTimestamp,
    ArchiveSquadRequest, ArchiveSquadResponse, AvailabilityDto, AvailabilitySourceDto,
    CreateSquadRequest, ErrorBody, ErrorEnvelope, HeartbeatRequest, HeartbeatResponse, InboxQuery,
    InboxResponse, IssuedSessionHeaders, JoinSquadRequest, LeaveSquadRequest, LeaveSquadResponse,
    MembershipStateDto, MessageDto, MessagePriorityDto, MessageSequence, ReadyResponse,
    ResumeSquadRequest, RosterResponse, SendMessageRequest, SendMessageResponse, SessionCredential,
    SessionResponse, SquadStateDto, SquadSummary, TranscriptQuery, TranscriptResponse,
    TransportPresenceDto, Validate, encode_bounded_inbox,
};
use psst_store::{
    AuthenticatedSession, CreateSquad, InboxPage, InstanceRecord, JoinAndClaim,
    JoinAndClaimOutcome, JoinMembership, LeasePolicy, LeaveOutcome, MembershipRecord, MessageView,
    RepositoryError, RosterMember, SendByName, SendOutcome, SessionContext, SquadRecord,
    TranscriptByName, TransportPresence,
};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot, watch};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

pub const DEFAULT_QUEUE_CAPACITY: usize = 256;
const MAX_INBOX_WAITERS: usize = 128;
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

struct NotificationRegistry {
    entries: Mutex<BTreeMap<String, NotificationEntry>>,
    permits: Arc<Semaphore>,
}

struct NotificationEntry {
    generation: watch::Sender<u64>,
    registrations: usize,
}

struct NotificationSubscription {
    registry: Arc<NotificationRegistry>,
    key: String,
    receiver: watch::Receiver<u64>,
    _permit: OwnedSemaphorePermit,
}

impl NotificationRegistry {
    fn bounded() -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(BTreeMap::new()),
            permits: Arc::new(Semaphore::new(MAX_INBOX_WAITERS)),
        })
    }

    fn subscribe(self: &Arc<Self>, membership: &MembershipId) -> Option<NotificationSubscription> {
        let permit = Arc::clone(&self.permits).try_acquire_owned().ok()?;
        let key = membership.to_string();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = entries.entry(key.clone()).or_insert_with(|| {
            let (generation, _) = watch::channel(0);
            NotificationEntry {
                generation,
                registrations: 0,
            }
        });
        entry.registrations += 1;
        let receiver = entry.generation.subscribe();
        Some(NotificationSubscription {
            registry: Arc::clone(self),
            key,
            receiver,
            _permit: permit,
        })
    }

    fn notify(&self, membership: &MembershipId) {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = entries.get(membership.as_str()) {
            entry
                .generation
                .send_modify(|value| *value = value.wrapping_add(1));
        }
    }

    #[cfg(any(test, feature = "reliability-test-support"))]
    fn registration_count(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|entry| entry.registrations)
            .sum()
    }
}

impl Drop for NotificationSubscription {
    fn drop(&mut self) {
        let mut entries = self
            .registry
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = entries.get_mut(&self.key) {
            entry.registrations -= 1;
            if entry.registrations == 0 {
                entries.remove(&self.key);
            }
        }
    }
}

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
            max_body_bytes: 512 * 1024,
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
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .try_init()?,
        LogFormat::Json => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(std::io::stderr),
            )
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

/// Operational facts emitted only after the database is open and the listener is bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelayStartup {
    pub bind: SocketAddr,
    pub database: PathBuf,
    pub schema_version: u32,
    pub trusted_lan_warning: Option<&'static str>,
}

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
    #[cfg(test)]
    DelayNextSendReply(
        Duration,
        std::sync::mpsc::Sender<()>,
        oneshot::Sender<Result<(), WorkerError>>,
    ),
    Join(
        WorkerJoin,
        oneshot::Sender<Result<JoinAndClaimOutcome, RepositoryError>>,
    ),
    ListSquads(oneshot::Sender<Result<Vec<SquadRecord>, RepositoryError>>),
    CreateSquad(
        WorkerCreateSquad,
        oneshot::Sender<Result<SquadRecord, RepositoryError>>,
    ),
    DescribeSquad(
        SquadName,
        oneshot::Sender<Result<SquadRecord, RepositoryError>>,
    ),
    Roster(
        SquadName,
        oneshot::Sender<Result<Vec<RosterMember>, RepositoryError>>,
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
        SendByName,
        oneshot::Sender<Result<SendOutcome, RepositoryError>>,
    ),
    Pending(
        Credential,
        usize,
        oneshot::Sender<Result<InboxPage, RepositoryError>>,
    ),
    Acknowledge(
        Credential,
        Vec<MessageId>,
        oneshot::Sender<Result<(), RepositoryError>>,
    ),
    Transcript(
        Credential,
        TranscriptByName,
        oneshot::Sender<Result<Vec<MessageView>, RepositoryError>>,
    ),
    Leave(
        Credential,
        SquadName,
        oneshot::Sender<Result<LeaveOutcome, RepositoryError>>,
    ),
    Archive(
        Credential,
        SquadName,
        oneshot::Sender<Result<SquadRecord, RepositoryError>>,
    ),
    Resume(
        WorkerResume,
        oneshot::Sender<Result<SessionContext, RepositoryError>>,
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
            #[cfg(test)]
            Self::DelayNextSendReply(_, _, reply) => {
                let _ = reply.send(Err(WorkerError::Unavailable));
            }
            Self::Ready(reply) | Self::Checkpoint(reply) => {
                let _ = reply.send(Err(WorkerError::Unavailable));
            }
            Self::Join(_, reply) => drop(reply),
            Self::ListSquads(reply) => drop(reply),
            Self::CreateSquad(_, reply) => drop(reply),
            Self::DescribeSquad(_, reply) => drop(reply),
            Self::Roster(_, reply) => drop(reply),
            Self::Heartbeat(_, _, _, _, reply) => drop(reply),
            Self::Resume(_, reply) => drop(reply),
            Self::Send(_, _, reply) => drop(reply),
            Self::Pending(_, _, reply) => drop(reply),
            Self::Transcript(_, _, reply) => drop(reply),
            Self::Acknowledge(_, _, reply) => drop(reply),
            Self::Leave(_, _, reply) => drop(reply),
            Self::Archive(_, _, reply) => drop(reply),
        }
    }
}

enum SendFailure {
    Full,
    Disconnected,
}

/// Owned adapter credential used only across the in-process worker boundary.
#[derive(Clone)]
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

pub struct WorkerCreateSquad {
    pub id: SquadId,
    pub name: SquadName,
    pub mission: Mission,
}

pub struct WorkerResume {
    pub prior: Credential,
    pub squad: SquadName,
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
    notifications: Arc<NotificationRegistry>,
    #[cfg(test)]
    inbox_gap: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
}

#[allow(clippy::missing_errors_doc)]
impl StoreWorker {
    /// Returns the number of admitted long-poll waiters for reliability tests.
    #[cfg(feature = "reliability-test-support")]
    #[doc(hidden)]
    #[must_use]
    pub fn reliability_active_inbox_waiters(&self) -> usize {
        self.inner.notifications.registration_count()
    }
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
        let notifications = NotificationRegistry::bounded();
        let worker_notifications = Arc::clone(&notifications);
        let worker_shutdown = Arc::clone(&shutdown);
        let handle = std::thread::Builder::new()
            .name("psst-sqlite".into())
            .spawn(move || {
                let mut store = store;
                #[cfg(test)]
                let mut next_send_reply_delay = None;
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
                        #[cfg(test)]
                        StoreCommand::DelayNextSendReply(delay, completed, reply) => {
                            next_send_reply_delay = Some((delay, completed));
                            let _ = reply.send(Ok(()));
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
                        StoreCommand::ListSquads(reply) => {
                            let _ = reply.send(store.list_squads());
                        }
                        StoreCommand::CreateSquad(request, reply) => {
                            let result = store.create_squad(&CreateSquad {
                                id: request.id,
                                name: request.name,
                                mission: request.mission,
                                created_at: clock.now(),
                            });
                            let _ = reply.send(result);
                        }
                        StoreCommand::DescribeSquad(name, reply) => {
                            let _ = reply.send(store.describe_squad(&name));
                        }
                        StoreCommand::Roster(name, reply) => {
                            let _ = reply.send(store.roster(&name, clock.now()));
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
                        StoreCommand::Send(credential, request, reply) => {
                            let now = clock.now();
                            let session = session(&credential, now);
                            let result = store.authenticated_send_by_name(&session, &request);
                            if let Ok(outcome) = &result {
                                worker_notifications
                                    .notify(&outcome.message.message.semantics.recipient);
                            }
                            #[cfg(test)]
                            let delayed_completion =
                                next_send_reply_delay.take().map(|(delay, completed)| {
                                    std::thread::sleep(delay);
                                    completed
                                });
                            let _ = reply.send(result);
                            #[cfg(test)]
                            if let Some(completed) = delayed_completion {
                                let _ = completed.send(());
                            }
                        }
                        StoreCommand::Pending(credential, limit, reply) => {
                            let session = session(&credential, clock.now());
                            let _ = reply.send(store.authenticated_pending_page(&session, limit));
                        }
                        StoreCommand::Acknowledge(credential, ids, reply) => {
                            let session = session(&credential, clock.now());
                            let _ = reply.send(store.authenticated_acknowledge(&session, ids));
                        }
                        StoreCommand::Transcript(credential, query, reply) => {
                            let session = session(&credential, clock.now());
                            let _ =
                                reply.send(store.authenticated_transcript_views(&session, &query));
                        }
                        StoreCommand::Leave(credential, squad, reply) => {
                            let session = session(&credential, clock.now());
                            let _ = reply.send(store.authenticated_leave(&session, &squad));
                        }
                        StoreCommand::Archive(credential, squad, reply) => {
                            let session = session(&credential, clock.now());
                            let _ =
                                reply.send(store.authenticated_archive_by_name(&session, &squad));
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
                                &request.squad,
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
                    notifications,
                    #[cfg(test)]
                    inbox_gap: Mutex::new(None),
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

    #[cfg(test)]
    async fn delay_next_send_reply(
        &self,
        delay: Duration,
    ) -> Result<std::sync::mpsc::Receiver<()>, WorkerError> {
        let (completed, completion) = std::sync::mpsc::channel();
        self.request(|reply| StoreCommand::DelayNextSendReply(delay, completed, reply))
            .await?;
        Ok(completion)
    }

    pub async fn join(&self, request: WorkerJoin) -> Result<JoinAndClaimOutcome, DispatchError> {
        self.dispatch(|reply| StoreCommand::Join(request, reply))
            .await
    }
    pub async fn list_squads(&self) -> Result<Vec<SquadRecord>, DispatchError> {
        self.dispatch(StoreCommand::ListSquads).await
    }
    pub async fn create_squad(
        &self,
        request: WorkerCreateSquad,
    ) -> Result<SquadRecord, DispatchError> {
        self.dispatch(|reply| StoreCommand::CreateSquad(request, reply))
            .await
    }
    pub async fn describe_squad(&self, name: SquadName) -> Result<SquadRecord, DispatchError> {
        self.dispatch(|reply| StoreCommand::DescribeSquad(name, reply))
            .await
    }
    pub async fn roster(&self, name: SquadName) -> Result<Vec<RosterMember>, DispatchError> {
        self.dispatch(|reply| StoreCommand::Roster(name, reply))
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
        request: SendByName,
    ) -> Result<SendOutcome, DispatchError> {
        self.dispatch(|reply| StoreCommand::Send(credential, request, reply))
            .await
    }
    pub async fn pending(
        &self,
        credential: Credential,
        limit: usize,
    ) -> Result<InboxPage, DispatchError> {
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
        query: TranscriptByName,
    ) -> Result<Vec<MessageView>, DispatchError> {
        self.dispatch(|reply| StoreCommand::Transcript(credential, query, reply))
            .await
    }
    pub async fn leave(
        &self,
        credential: Credential,
        squad: SquadName,
    ) -> Result<LeaveOutcome, DispatchError> {
        self.dispatch(|reply| StoreCommand::Leave(credential, squad, reply))
            .await
    }
    pub async fn archive(
        &self,
        credential: Credential,
        squad: SquadName,
    ) -> Result<SquadRecord, DispatchError> {
        self.dispatch(|reply| StoreCommand::Archive(credential, squad, reply))
            .await
    }
    pub async fn resume(&self, request: WorkerResume) -> Result<SessionContext, DispatchError> {
        self.dispatch(|reply| StoreCommand::Resume(request, reply))
            .await
    }

    #[cfg(test)]
    fn pause_next_inbox_after_preflight(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        *self
            .inner
            .inbox_gap
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((started_tx, release_rx));
        (started_rx, release_tx)
    }

    #[cfg(test)]
    async fn inbox_preflight_barrier(&self) {
        let hook = self
            .inner
            .inbox_gap
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((started, release)) = hook {
            let _ = started.send(());
            let _ = release.await;
        }
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
    shutdown: watch::Receiver<bool>,
}

#[derive(Serialize)]
struct StatusBody {
    status: &'static str,
}

pub fn router(worker: StoreWorker) -> Router {
    let (_shutdown, receiver) = watch::channel(false);
    router_with_limits_and_shutdown(worker, 512 * 1024, 128, Duration::from_secs(5), receiver)
}

pub fn router_with_limits(
    worker: StoreWorker,
    max_body: usize,
    max_in_flight: usize,
    timeout: Duration,
) -> Router {
    let (_shutdown, receiver) = watch::channel(false);
    router_with_limits_and_shutdown(worker, max_body, max_in_flight, timeout, receiver)
}

fn router_with_limits_and_shutdown(
    worker: StoreWorker,
    max_body: usize,
    max_in_flight: usize,
    timeout: Duration,
    shutdown: watch::Receiver<bool>,
) -> Router {
    apply_limits(
        Router::new()
            .route("/healthz", get(health))
            .route("/readyz", get(ready))
            .route("/v1/squads", get(list_squads).post(create_squad))
            .route("/v1/squads/{squad}", get(describe_squad))
            .route("/v1/squads/{squad}/archive", post(archive_squad))
            .route("/v1/squads/{squad}/join", post(join_squad))
            .route("/v1/squads/{squad}/resume", post(resume_squad))
            .route("/v1/squads/{squad}/leave", post(leave_squad))
            .route("/v1/squads/{squad}/roster", get(roster))
            .route("/v1/heartbeat", post(heartbeat))
            .route("/v1/messages", post(send_message))
            .route("/v1/inbox", get(inbox))
            .route("/v1/messages/ack", post(acknowledge_messages))
            .route("/v1/squads/{squad}/transcript", get(transcript))
            .with_state(AppState { worker, shutdown }),
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
    let allowance = inbox_wait_allowance(request.uri()).unwrap_or_default();
    let timeout = timeout.saturating_add(allowance);
    tokio::time::timeout(timeout, next.run(request))
        .await
        .unwrap_or_else(|_| StatusCode::REQUEST_TIMEOUT.into_response())
}

fn inbox_wait_allowance(uri: &axum::http::Uri) -> Option<Duration> {
    if uri.path() != "/v1/inbox" {
        return None;
    }
    let mut wait = None;
    for pair in uri.query()?.split('&') {
        let (name, value) = pair.split_once('=')?;
        if name == "wait" {
            if wait.is_some()
                || value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            let seconds = value.parse::<u8>().ok()?;
            if seconds > 30 {
                return None;
            }
            wait = Some(seconds);
        }
    }
    wait.map(|seconds| Duration::from_secs(u64::from(seconds)))
}

async fn health() -> Json<StatusBody> {
    Json(StatusBody { status: "ok" })
}
async fn ready(State(state): State<AppState>) -> (StatusCode, Json<ReadyResponse>) {
    match state.worker.ready().await {
        Ok(()) => (
            StatusCode::OK,
            Json(ReadyResponse {
                status: "ready".into(),
                schema_version: psst_store::current_schema_version(),
            }),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyResponse {
                status: "unavailable".into(),
                schema_version: psst_store::current_schema_version(),
            }),
        ),
    }
}

#[derive(Debug)]
struct ApiFailure(ApiErrorCode);

struct ApiJson<T>(T);
impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = ApiFailure;
    async fn from_request(request: Request<Body>, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|rejection| {
                ApiFailure(if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    ApiErrorCode::PayloadTooLarge
                } else {
                    ApiErrorCode::InvalidRequest
                })
            })
    }
}
struct ApiQuery<T>(T);
impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = ApiFailure;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|_| ApiFailure(ApiErrorCode::InvalidRequest))
    }
}
impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        let retryable = matches!(
            self.0,
            ApiErrorCode::RateLimited | ApiErrorCode::DatabaseBusy
        );
        let status =
            StatusCode::from_u16(self.0.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let message = match self.0 {
            ApiErrorCode::InvalidRequest => "The request is invalid.",
            ApiErrorCode::NotFound => "The requested resource was not found.",
            ApiErrorCode::SquadArchived => "The squad is archived.",
            ApiErrorCode::NotMember => "The session is not an active member.",
            ApiErrorCode::NameInUse => "The requested name is in use.",
            ApiErrorCode::LeaseExpired => "The session lease has expired.",
            ApiErrorCode::RecipientNotFound => "The recipient was not found.",
            ApiErrorCode::IdempotencyConflict => "The idempotency key conflicts.",
            ApiErrorCode::PayloadTooLarge => "The payload is too large.",
            ApiErrorCode::RateLimited => "The relay is busy; retry later.",
            ApiErrorCode::DatabaseBusy => "The database is busy; retry later.",
            ApiErrorCode::InternalError => "The relay encountered an internal error.",
        };
        (
            status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.0,
                    message: message.into(),
                    retryable,
                    details: BTreeMap::default(),
                },
            }),
        )
            .into_response()
    }
}

impl From<DispatchError> for ApiFailure {
    fn from(error: DispatchError) -> Self {
        let code = match error {
            DispatchError::RateLimited => ApiErrorCode::RateLimited,
            DispatchError::Unavailable | DispatchError::Timeout => ApiErrorCode::DatabaseBusy,
            DispatchError::Store(error) => match error.code() {
                psst_core::ErrorCode::InvalidRequest => ApiErrorCode::InvalidRequest,
                psst_core::ErrorCode::NotFound => ApiErrorCode::NotFound,
                psst_core::ErrorCode::SquadArchived => ApiErrorCode::SquadArchived,
                psst_core::ErrorCode::NotMember => ApiErrorCode::NotMember,
                psst_core::ErrorCode::NameInUse => ApiErrorCode::NameInUse,
                psst_core::ErrorCode::LeaseExpired => ApiErrorCode::LeaseExpired,
                psst_core::ErrorCode::RecipientNotFound => ApiErrorCode::RecipientNotFound,
                psst_core::ErrorCode::IdempotencyConflict => ApiErrorCode::IdempotencyConflict,
                psst_core::ErrorCode::PayloadTooLarge => ApiErrorCode::PayloadTooLarge,
                psst_core::ErrorCode::RateLimited => ApiErrorCode::RateLimited,
                psst_core::ErrorCode::DatabaseBusy => ApiErrorCode::DatabaseBusy,
                _ => ApiErrorCode::InternalError,
            },
        };
        Self(code)
    }
}

fn parsed<T>(value: Result<T, psst_core::InvalidValue>) -> Result<T, ApiFailure> {
    value.map_err(|_| ApiFailure(ApiErrorCode::InvalidRequest))
}
fn credential(headers: &HeaderMap) -> Result<Credential, ApiFailure> {
    if headers.get_all(AUTHORIZATION).iter().count() != 1 {
        return Err(ApiFailure(ApiErrorCode::NotFound));
    }
    let value = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiFailure(ApiErrorCode::NotFound))?;
    let parsed = SessionCredential::parse_authorization(value)
        .map_err(|_| ApiFailure(ApiErrorCode::NotFound))?;
    Ok(Credential {
        instance_id: parsed.instance_id().clone(),
        resume_token: parsed.resume_token().clone(),
    })
}
fn identifier<T>(
    prefix: &str,
    make: impl FnOnce(String) -> Result<T, psst_core::InvalidValue>,
) -> Result<T, ApiFailure> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|_| ApiFailure(ApiErrorCode::InternalError))?;
    let suffix = random
        .iter()
        .fold(String::with_capacity(32), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        });
    parsed(make(format!("{prefix}_{suffix}")))
}
fn timestamp(value: UnixMillis) -> Result<ApiTimestamp, ApiFailure> {
    let nanos = i128::from(value.as_i64()) * 1_000_000;
    let time = time::OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .map_err(|_| ApiFailure(ApiErrorCode::InternalError))?;
    ApiTimestamp::new(time).map_err(|_| ApiFailure(ApiErrorCode::InternalError))
}
fn squad_dto(value: &SquadRecord) -> Result<SquadSummary, ApiFailure> {
    Ok(SquadSummary {
        id: value.id.to_string(),
        name: value.name.to_string(),
        mission: value.mission.to_string(),
        state: match value.state {
            SquadState::Active => SquadStateDto::Active,
            SquadState::Archived => SquadStateDto::Archived,
        },
        created_at: timestamp(value.created_at)?,
        archived_at: value.archived_at.map(timestamp).transpose()?,
    })
}
fn mode(value: AgentModeDto) -> AgentMode {
    match value {
        AgentModeDto::Cooperative => AgentMode::Cooperative,
        AgentModeDto::Scheduled => AgentMode::Scheduled,
        AgentModeDto::Harnessed => AgentMode::Harnessed,
    }
}
fn availability(value: AvailabilityDto) -> Availability {
    match value {
        AvailabilityDto::Idle => Availability::Idle,
        AvailabilityDto::Busy => Availability::Busy,
        AvailabilityDto::Blocked => Availability::Blocked,
        AvailabilityDto::Unknown => Availability::Unknown,
    }
}
fn availability_source(value: AvailabilitySourceDto) -> AvailabilitySource {
    match value {
        AvailabilitySourceDto::SessionLifecycle => AvailabilitySource::SessionLifecycle,
        AvailabilitySourceDto::McpConnection => AvailabilitySource::McpConnection,
        AvailabilitySourceDto::ToolActivity => AvailabilitySource::ToolActivity,
        AvailabilitySourceDto::AgentReported => AvailabilitySource::AgentReported,
        AvailabilitySourceDto::Unknown => AvailabilitySource::Unknown,
    }
}
fn mode_dto(value: AgentMode) -> AgentModeDto {
    match value {
        AgentMode::Cooperative => AgentModeDto::Cooperative,
        AgentMode::Scheduled => AgentModeDto::Scheduled,
        AgentMode::Harnessed => AgentModeDto::Harnessed,
    }
}
fn availability_dto(value: Availability) -> AvailabilityDto {
    match value {
        Availability::Idle => AvailabilityDto::Idle,
        Availability::Busy => AvailabilityDto::Busy,
        Availability::Blocked => AvailabilityDto::Blocked,
        Availability::Unknown => AvailabilityDto::Unknown,
    }
}
fn source_dto(value: AvailabilitySource) -> AvailabilitySourceDto {
    match value {
        AvailabilitySource::SessionLifecycle => AvailabilitySourceDto::SessionLifecycle,
        AvailabilitySource::McpConnection => AvailabilitySourceDto::McpConnection,
        AvailabilitySource::ToolActivity => AvailabilitySourceDto::ToolActivity,
        AvailabilitySource::AgentReported => AvailabilitySourceDto::AgentReported,
        AvailabilitySource::Unknown => AvailabilitySourceDto::Unknown,
    }
}
fn seconds(value: Duration) -> Result<u32, ApiFailure> {
    u32::try_from(value.as_secs()).map_err(|_| ApiFailure(ApiErrorCode::InternalError))
}
fn session_response(
    membership: &MembershipRecord,
    squad: &SquadRecord,
    instance: &InstanceRecord,
) -> Result<SessionResponse, ApiFailure> {
    Ok(SessionResponse {
        agent_id: membership.agent_id.to_string(),
        membership_id: membership.id.to_string(),
        instance_id: instance.id.to_string(),
        squad: squad_dto(squad)?,
        member_name: membership.name.to_string(),
        role: membership.role.to_string(),
        heartbeat_interval_seconds: seconds(instance.heartbeat_interval)?,
        lease_seconds: seconds(instance.lease_duration)?,
        lease_expires_at: timestamp(instance.lease_expires_at)?,
    })
}
fn issued_response(
    body: SessionResponse,
    instance: &InstanceId,
    token: &ResumeToken,
) -> Result<Response, ApiFailure> {
    let authority =
        SessionCredential::parse_session_value(&format!("{instance}.{}", token.expose_encoded()))
            .map_err(|_| ApiFailure(ApiErrorCode::InternalError))?;
    let issued = IssuedSessionHeaders::new(&authority)
        .map_err(|_| ApiFailure(ApiErrorCode::InternalError))?;
    let mut response = Json(body).into_response();
    issued.apply(response.headers_mut());
    Ok(response)
}

async fn list_squads(State(state): State<AppState>) -> Result<Json<Vec<SquadSummary>>, ApiFailure> {
    let values = state.worker.list_squads().await?;
    Ok(Json(
        values
            .into_iter()
            .map(|value| squad_dto(&value))
            .collect::<Result<_, _>>()?,
    ))
}
async fn create_squad(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<CreateSquadRequest>,
) -> Result<Json<SquadSummary>, ApiFailure> {
    request
        .validate()
        .map_err(|_| ApiFailure(ApiErrorCode::InvalidRequest))?;
    let result = state
        .worker
        .create_squad(WorkerCreateSquad {
            id: identifier("sqd", SquadId::new)?,
            name: parsed(SquadName::new(request.name))?,
            mission: parsed(Mission::new(request.mission))?,
        })
        .await?;
    Ok(Json(squad_dto(&result)?))
}
async fn describe_squad(
    State(state): State<AppState>,
    AxumPath(squad): AxumPath<String>,
) -> Result<Json<SquadSummary>, ApiFailure> {
    let result = state
        .worker
        .describe_squad(parsed(SquadName::new(squad))?)
        .await?;
    Ok(Json(squad_dto(&result)?))
}
async fn join_squad(
    State(state): State<AppState>,
    AxumPath(squad): AxumPath<String>,
    ApiJson(request): ApiJson<JoinSquadRequest>,
) -> Result<Response, ApiFailure> {
    request
        .validate()
        .map_err(|_| ApiFailure(ApiErrorCode::InvalidRequest))?;
    let squad_name = parsed(SquadName::new(squad))?;
    let outcome = state
        .worker
        .join(WorkerJoin {
            membership: JoinMembership {
                squad_name,
                mission_if_missing: request
                    .mission
                    .map(Mission::new)
                    .transpose()
                    .map_err(|_| ApiFailure(ApiErrorCode::InvalidRequest))?,
                squad_id_if_missing: identifier("sqd", SquadId::new)?,
                agent_id: identifier("agt", AgentId::new)?,
                membership_id: identifier("mem", MembershipId::new)?,
                member_name: parsed(MemberName::new(request.name))?,
                role: parsed(Role::new(request.role))?,
                joined_at: UnixMillis::new(0).expect("zero valid"),
            },
            instance_id: identifier("ins", InstanceId::new)?,
            mode: mode(request.mode),
            client_kind: request.client.kind,
            hostname: request.client.hostname,
            availability: Availability::Unknown,
            availability_source: AvailabilitySource::Unknown,
            lease_policy: LeasePolicy::default(),
        })
        .await?;
    let (membership, squad, instance, token) = outcome.into_session_parts();
    let body = session_response(&membership, &squad, &instance)?;
    issued_response(body, &instance.id, &token)
}
async fn resume_squad(
    State(state): State<AppState>,
    AxumPath(squad): AxumPath<String>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<ResumeSquadRequest>,
) -> Result<Response, ApiFailure> {
    request
        .validate()
        .map_err(|_| ApiFailure(ApiErrorCode::InvalidRequest))?;
    let prior = credential(&headers)?;
    let token = prior.resume_token.clone();
    let context = state
        .worker
        .resume(WorkerResume {
            prior,
            squad: parsed(SquadName::new(squad))?,
            new_instance: identifier("ins", InstanceId::new)?,
            mode: mode(request.mode),
            client_kind: request.client.kind,
            hostname: request.client.hostname,
            availability: Availability::Unknown,
            availability_source: AvailabilitySource::Unknown,
            lease_policy: LeasePolicy::default(),
        })
        .await?;
    let body = session_response(&context.membership, &context.squad, &context.instance)?;
    issued_response(body, &context.instance.id, &token)
}
async fn leave_squad(
    State(state): State<AppState>,
    AxumPath(squad): AxumPath<String>,
    headers: HeaderMap,
    ApiJson(_): ApiJson<LeaveSquadRequest>,
) -> Result<Json<LeaveSquadResponse>, ApiFailure> {
    let result = state
        .worker
        .leave(credential(&headers)?, parsed(SquadName::new(squad))?)
        .await?;
    Ok(Json(LeaveSquadResponse {
        membership_id: result.membership_id.to_string(),
        left_at: timestamp(result.left_at)?,
    }))
}
async fn archive_squad(
    State(state): State<AppState>,
    AxumPath(squad): AxumPath<String>,
    headers: HeaderMap,
    ApiJson(_): ApiJson<ArchiveSquadRequest>,
) -> Result<Json<ArchiveSquadResponse>, ApiFailure> {
    let result = state
        .worker
        .archive(credential(&headers)?, parsed(SquadName::new(squad))?)
        .await?;
    Ok(Json(ArchiveSquadResponse {
        squad: squad_dto(&result)?,
    }))
}
async fn roster(
    State(state): State<AppState>,
    AxumPath(squad): AxumPath<String>,
) -> Result<Json<RosterResponse>, ApiFailure> {
    let name = parsed(SquadName::new(squad))?;
    let members = state.worker.roster(name.clone()).await?;
    let members = members
        .into_iter()
        .map(|value| {
            Ok(psst_protocol::RosterMember {
                membership_id: value.membership.id.to_string(),
                name: value.membership.name.to_string(),
                role: value.membership.role.to_string(),
                membership_state: if value.membership.left_at.is_some() {
                    MembershipStateDto::Left
                } else {
                    MembershipStateDto::Joined
                },
                presence: match value.presence {
                    TransportPresence::Online => TransportPresenceDto::Online,
                    TransportPresence::Offline => TransportPresenceDto::Offline,
                },
                availability: availability_dto(value.availability.availability()),
                availability_source: source_dto(value.availability.source()),
                availability_observed_at: timestamp(value.availability.observed_at())?,
                mode: value.mode.map(mode_dto),
                last_seen_at: value.last_seen_at.map(timestamp).transpose()?,
            })
        })
        .collect::<Result<Vec<_>, ApiFailure>>()?;
    Ok(Json(RosterResponse {
        squad: name.to_string(),
        members,
    }))
}
async fn heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<HeartbeatRequest>,
) -> Result<Json<HeartbeatResponse>, ApiFailure> {
    request
        .validate()
        .map_err(|_| ApiFailure(ApiErrorCode::InvalidRequest))?;
    let record = state
        .worker
        .heartbeat(
            credential(&headers)?,
            availability(request.availability),
            availability_source(request.availability_source),
            LeasePolicy::default(),
        )
        .await?;
    Ok(Json(HeartbeatResponse {
        lease_expires_at: timestamp(record.lease_expires_at)?,
        heartbeat_interval_seconds: seconds(record.heartbeat_interval)?,
    }))
}

fn priority(value: MessagePriorityDto) -> MessagePriority {
    match value {
        MessagePriorityDto::Normal => MessagePriority::Normal,
        MessagePriorityDto::High => MessagePriority::High,
    }
}

fn priority_dto(value: MessagePriority) -> MessagePriorityDto {
    match value {
        MessagePriority::Normal => MessagePriorityDto::Normal,
        MessagePriority::High => MessagePriorityDto::High,
    }
}

fn message_dto(value: MessageView) -> Result<MessageDto, ApiFailure> {
    Ok(MessageDto {
        sequence: MessageSequence::new(value.message.sequence)
            .map_err(|_| ApiFailure(ApiErrorCode::InternalError))?,
        id: value.message.id.to_string(),
        squad: value.squad.to_string(),
        sender: value.sender.to_string(),
        recipient: value.recipient.to_string(),
        body: value.message.semantics.body.as_str().to_owned(),
        priority: priority_dto(value.message.semantics.priority),
        reply_to: value.message.semantics.reply_to.map(|id| id.to_string()),
        correlation_id: value
            .message
            .semantics
            .correlation_id
            .map(|id| id.to_string()),
        created_at: timestamp(value.message.created_at)?,
        acknowledged_at: value.acknowledged_at.map(timestamp).transpose()?,
    })
}

async fn send_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, ApiFailure> {
    if request.body.len() > MessageBody::MAX_BYTES {
        return Err(ApiFailure(ApiErrorCode::PayloadTooLarge));
    }
    request
        .validate()
        .map_err(|_| ApiFailure(ApiErrorCode::InvalidRequest))?;
    let outcome = state
        .worker
        .send(
            credential(&headers)?,
            SendByName {
                id: identifier("msg", MessageId::new)?,
                recipient: parsed(MemberName::new(request.recipient))?,
                body: parsed(MessageBody::new(request.body))?,
                priority: priority(request.priority),
                dedupe_key: parsed(DedupeKey::new(request.dedupe_key))?,
                reply_to: request
                    .reply_to
                    .map(MessageId::new)
                    .transpose()
                    .map_err(|_| ApiFailure(ApiErrorCode::InvalidRequest))?,
                correlation_id: request
                    .correlation_id
                    .map(CorrelationId::new)
                    .transpose()
                    .map_err(|_| ApiFailure(ApiErrorCode::InvalidRequest))?,
            },
        )
        .await?;
    Ok(Json(SendMessageResponse {
        message: message_dto(outcome.message)?,
        idempotent_replay: outcome.idempotent_replay,
    }))
}

async fn inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<InboxQuery>,
) -> Result<Response, ApiFailure> {
    if !(1..=100).contains(&query.limit) || query.wait_seconds > 30 {
        return Err(ApiFailure(ApiErrorCode::InvalidRequest));
    }
    let credential = credential(&headers)?;
    let mut page = state
        .worker
        .pending(credential.clone(), usize::from(query.limit))
        .await?;
    #[cfg(test)]
    state.worker.inbox_preflight_barrier().await;
    if page.pending_count == 0 && query.wait_seconds > 0 {
        let mut subscription = state
            .worker
            .inner
            .notifications
            .subscribe(&page.recipient_membership)
            .ok_or(ApiFailure(ApiErrorCode::RateLimited))?;

        // Register before the second durable check. A send in the preflight-to-
        // registration gap is observed by this query; subsequent sends advance
        // the watch generation, which cannot be lost even when coalesced.
        page = state
            .worker
            .pending(credential.clone(), usize::from(query.limit))
            .await?;
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(u64::from(query.wait_seconds));
        while page.pending_count == 0 {
            tokio::select! {
                biased;
                () = wait_for_shutdown(state.shutdown.clone()) => {
                    return Err(ApiFailure(ApiErrorCode::DatabaseBusy));
                }
                () = tokio::time::sleep_until(deadline) => break,
                changed = subscription.receiver.changed() => {
                    if changed.is_err() {
                        return Err(ApiFailure(ApiErrorCode::DatabaseBusy));
                    }
                    page = state
                        .worker
                        .pending(credential.clone(), usize::from(query.limit))
                        .await?;
                }
            }
        }
    }
    encode_inbox(page)
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if shutdown.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

fn encode_inbox(page: InboxPage) -> Result<Response, ApiFailure> {
    let response = InboxResponse {
        messages: page
            .messages
            .into_iter()
            .map(message_dto)
            .collect::<Result<Vec<_>, _>>()?,
        pending_count: page.pending_count,
    };
    let encoded =
        encode_bounded_inbox(&response).map_err(|_| ApiFailure(ApiErrorCode::PayloadTooLarge))?;
    Ok((
        [("content-type", psst_protocol::JSON_CONTENT_TYPE)],
        encoded,
    )
        .into_response())
}

async fn acknowledge_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<AckMessagesRequest>,
) -> Result<Json<AckMessagesResponse>, ApiFailure> {
    request
        .validate()
        .map_err(|_| ApiFailure(ApiErrorCode::InvalidRequest))?;
    let mut seen = HashSet::with_capacity(request.message_ids.len());
    let mut acknowledged_ids = Vec::with_capacity(request.message_ids.len());
    let mut ids = Vec::with_capacity(request.message_ids.len());
    for value in request.message_ids {
        let id = parsed(MessageId::new(value.clone()))?;
        if seen.insert(id.clone()) {
            acknowledged_ids.push(value);
            ids.push(id);
        }
    }
    state.worker.acknowledge(credential(&headers)?, ids).await?;
    Ok(Json(AckMessagesResponse { acknowledged_ids }))
}

async fn transcript(
    State(state): State<AppState>,
    AxumPath(squad): AxumPath<String>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<TranscriptQuery>,
) -> Result<Json<TranscriptResponse>, ApiFailure> {
    if !(1..=100).contains(&query.limit) {
        return Err(ApiFailure(ApiErrorCode::InvalidRequest));
    }
    let messages = state
        .worker
        .transcript(
            credential(&headers)?,
            TranscriptByName {
                squad: parsed(SquadName::new(squad))?,
                after: query.after.value(),
                limit: usize::from(query.limit),
            },
        )
        .await?
        .into_iter()
        .map(message_dto)
        .collect::<Result<Vec<_>, _>>()?;
    let next_after = messages.last().map(|message| message.sequence);
    Ok(Json(TranscriptResponse {
        messages,
        next_after,
    }))
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
    serve_with_router_factory(config, shutdown, None, |worker, config, shutdown| {
        router_with_limits_and_shutdown(
            worker,
            config.max_body_bytes,
            config.max_in_flight_requests,
            config.request_timeout,
            shutdown,
        )
    })
    .await
}

/// Runs the production relay and reports readiness after the database opens and listener binds.
///
/// # Errors
/// Returns the same bounded serving errors as [`serve`].
pub async fn serve_with_startup(
    config: RelayConfig,
    shutdown: watch::Receiver<bool>,
    startup: oneshot::Sender<RelayStartup>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_with_router_factory(
        config,
        shutdown,
        Some(startup),
        |worker, config, shutdown| {
            router_with_limits_and_shutdown(
                worker,
                config.max_body_bytes,
                config.max_in_flight_requests,
                config.request_timeout,
                shutdown,
            )
        },
    )
    .await
}

/// Runs the production server while exposing a read-only worker probe to reliability tests.
#[cfg(feature = "reliability-test-support")]
#[doc(hidden)]
pub async fn serve_with_reliability_probe(
    config: RelayConfig,
    shutdown: watch::Receiver<bool>,
    probe: oneshot::Sender<StoreWorker>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_with_router_factory(config, shutdown, None, move |worker, config, shutdown| {
        let _ = probe.send(worker.clone());
        router_with_limits_and_shutdown(
            worker,
            config.max_body_bytes,
            config.max_in_flight_requests,
            config.request_timeout,
            shutdown,
        )
    })
    .await
}

async fn serve_with_router_factory(
    config: RelayConfig,
    mut shutdown: watch::Receiver<bool>,
    startup: Option<oneshot::Sender<RelayStartup>>,
    make_router: impl FnOnce(StoreWorker, &RelayConfig, watch::Receiver<bool>) -> Router,
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
    tracing::info!(
        bind = %bound,
        database = %config.database.display(),
        schema_version = psst_store::current_schema_version(),
        "relay started"
    );
    if let Some(startup) = startup {
        let _ = startup.send(RelayStartup {
            bind: bound,
            database: config.database.clone(),
            schema_version: psst_store::current_schema_version(),
            trusted_lan_warning: config.trusted_lan_warning(),
        });
    }
    let shutdown_started = Arc::new(tokio::sync::Notify::new());
    let shutdown_notice = Arc::clone(&shutdown_started);
    let server = axum::serve(
        LimitedTcpListener::new(listener, config.max_connections),
        make_router(worker.clone(), &config, shutdown.clone()),
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

    async fn json_request(
        app: Router,
        request: Request<Body>,
    ) -> (StatusCode, HeaderMap, serde_json::Value) {
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap();
        (status, headers, body)
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn lifecycle_http_is_committed_path_bound_and_secret_safe() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start(&directory.path().join("psst.db"), 16).unwrap();
        let app = router(worker.clone());
        let (status, _, created) = json_request(
            app.clone(),
            Request::post("/v1/squads")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"name":"created","mission":"explicit lifecycle"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(created["name"], "created");
        let (status, _, listed) = json_request(
            app.clone(),
            Request::get("/v1/squads").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            listed
                .as_array()
                .unwrap()
                .iter()
                .any(|squad| squad["name"] == "created")
        );
        let (status, _, described) = json_request(
            app.clone(),
            Request::get("/v1/squads/created")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(described["mission"], "explicit lifecycle");
        let join_body = r#"{"name":"alice","role":"builder","mode":"cooperative","client":{"kind":"test"},"mission":"ship safely"}"#;
        let (status, headers, joined) = json_request(
            app.clone(),
            Request::post("/v1/squads/alpha/join")
                .header("content-type", "application/json")
                .body(Body::from(join_body))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get("cache-control").unwrap(), "no-store");
        assert!(
            headers
                .get("psst-session-credential")
                .unwrap()
                .is_sensitive()
        );
        let credential = headers
            .get("psst-session-credential")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(!joined.to_string().contains(&credential));
        assert_eq!(joined["squad"]["name"], "alpha");

        let (status, _, roster_body) = json_request(
            app.clone(),
            Request::get("/v1/squads/alpha/roster")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(roster_body["members"][0]["name"], "alice");

        let heartbeat_body = r#"{"availability":"busy","availability_source":"agent_reported"}"#;
        let (status, _, heartbeat_body) = json_request(
            app.clone(),
            Request::post("/v1/heartbeat")
                .header("authorization", format!("Bearer {credential}"))
                .header("content-type", "application/json")
                .body(Body::from(heartbeat_body))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(heartbeat_body["heartbeat_interval_seconds"], 10);

        let (status, _, wrong_path) = json_request(
            app.clone(),
            Request::post("/v1/squads/other/leave")
                .header("authorization", format!("Bearer {credential}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!wrong_path.to_string().contains(&credential));

        let resume_body = r#"{"mode":"cooperative","client":{"kind":"test"}}"#;
        let (status, _, wrong_resume) = json_request(
            app.clone(),
            Request::post("/v1/squads/other/resume")
                .header("authorization", format!("Bearer {credential}"))
                .header("content-type", "application/json")
                .body(Body::from(resume_body))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(wrong_resume["error"]["code"], "not_found");

        let (status, _, duplicate) = json_request(
            app.clone(),
            Request::post("/v1/heartbeat")
                .header("authorization", format!("Bearer {credential}"))
                .header("authorization", format!("Bearer {credential}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"availability":"busy","availability_source":"agent_reported"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(duplicate["error"]["code"], "not_found");

        let (_, leave_headers, _) = json_request(
            app.clone(),
            Request::post("/v1/squads/leavers/join")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"leaver","role":"test","mode":"cooperative","client":{"kind":"test"},"mission":"leave"}"#))
                .unwrap(),
        )
        .await;
        let leave_credential = leave_headers
            .get("psst-session-credential")
            .unwrap()
            .to_str()
            .unwrap();
        let (status, _, left) = json_request(
            app.clone(),
            Request::post("/v1/squads/leavers/leave")
                .header("authorization", format!("Bearer {leave_credential}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(left["membership_id"].as_str().unwrap().starts_with("mem_"));

        let (status, _, archived) = json_request(
            app.clone(),
            Request::post("/v1/squads/alpha/archive")
                .header("authorization", format!("Bearer {credential}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(archived["squad"]["state"], "archived");
        let (status, _, _) = json_request(
            app.clone(),
            Request::get("/v1/squads/alpha/roster")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "archived roster remains a trusted-LAN read"
        );

        let (status, _, missing) = json_request(app, Request::post("/v1/squads/missing/join").header("content-type", "application/json").body(Body::from(r#"{"name":"bob","role":"builder","mode":"cooperative","client":{"kind":"test"}}"#)).unwrap()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(missing["error"]["code"], "not_found");
        let (status, _, malformed) = json_request(
            router(worker.clone()),
            Request::post("/v1/heartbeat")
                .header("authorization", format!("Bearer {credential}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"availability":"busy","availability_source":"agent_reported","unexpected":true}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(malformed["error"]["code"], "invalid_request");
        for request in [
            Request::post("/v1/heartbeat")
                .header("authorization", format!("Bearer {credential}"))
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
            Request::post("/v1/heartbeat")
                .header("authorization", format!("Bearer {credential}"))
                .body(Body::from(
                    r#"{"availability":"busy","availability_source":"agent_reported"}"#,
                ))
                .unwrap(),
            Request::post("/v1/heartbeat")
                .header("authorization", format!("Bearer {credential}"))
                .header("content-type", "text/plain")
                .body(Body::from(
                    r#"{"availability":"busy","availability_source":"agent_reported"}"#,
                ))
                .unwrap(),
        ] {
            let (status, _, body) = json_request(router(worker.clone()), request).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["error"]["code"], "invalid_request");
        }
        let oversized = "x".repeat(256);
        let (status, _, oversized_body) = json_request(
            router_with_limits(worker.clone(), 64, 8, Duration::from_secs(1)),
            Request::post("/v1/heartbeat")
                .header("authorization", format!("Bearer {credential}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"availability":"busy","availability_source":"agent_reported","padding":"{oversized}"}}"#)))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(oversized_body["error"]["code"], "payload_too_large");
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn http_session_resumes_after_real_store_restart() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let (worker, handle) = StoreWorker::start_with_time(
            &path,
            8,
            Duration::from_secs(1),
            Arc::new(FakeTime(UnixMillis::new(100).unwrap())),
        )
        .unwrap();
        let join = Request::post("/v1/squads/alpha/join")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name":"alice","role":"builder","mode":"cooperative","client":{"kind":"test"},"mission":"restart"}"#))
            .unwrap();
        let (status, headers, joined) = json_request(router(worker.clone()), join).await;
        assert_eq!(status, StatusCode::OK);
        let old_instance = joined["instance_id"].as_str().unwrap().to_owned();
        let credential = headers
            .get("psst-session-credential")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();

        let (worker, handle) = StoreWorker::start_with_time(
            &path,
            8,
            Duration::from_secs(1),
            Arc::new(FakeTime(UnixMillis::new(40_000).unwrap())),
        )
        .unwrap();
        let resume = Request::post("/v1/squads/alpha/resume")
            .header("authorization", format!("Bearer {credential}"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"mode":"harnessed","client":{"kind":"test"}}"#,
            ))
            .unwrap();
        let (status, headers, resumed) = json_request(router(worker.clone()), resume).await;
        assert_eq!(status, StatusCode::OK);
        assert_ne!(resumed["instance_id"], old_instance);
        assert_eq!(headers.get("cache-control").unwrap(), "no-store");
        assert!(
            !resumed.to_string().contains(
                headers
                    .get("psst-session-credential")
                    .unwrap()
                    .to_str()
                    .unwrap()
            )
        );
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn lifecycle_http_authorization_matrix_is_concealed_and_expiry_is_stable() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let (worker, handle) = StoreWorker::start_with_time(
            &path,
            16,
            Duration::from_secs(1),
            Arc::new(FakeTime(UnixMillis::new(100).unwrap())),
        )
        .unwrap();
        let app = router(worker.clone());
        let join = Request::post("/v1/squads/alpha/join")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name":"alice","role":"builder","mode":"cooperative","client":{"kind":"test"},"mission":"matrix"}"#))
            .unwrap();
        let (_, headers, _) = json_request(app.clone(), join).await;
        let valid = headers
            .get("psst-session-credential")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let (valid_instance, valid_token) = valid.split_once('.').unwrap();
        let replacement = if valid_token.starts_with('A') {
            'B'
        } else {
            'A'
        };
        let mut wrong_token = valid_token.to_owned();
        wrong_token.replace_range(..1, &replacement.to_string());
        let wrong_token_authority = format!("Bearer {valid_instance}.{wrong_token}");
        let nonexistent_authority = format!("Bearer ins_nonexistent.{valid_token}");
        let cases = [
            (
                "/v1/heartbeat",
                r#"{"availability":"busy","availability_source":"agent_reported"}"#,
            ),
            ("/v1/squads/alpha/leave", "{}"),
            ("/v1/squads/alpha/archive", "{}"),
            (
                "/v1/squads/alpha/resume",
                r#"{"mode":"cooperative","client":{"kind":"test"}}"#,
            ),
        ];
        for (path, body) in cases {
            for authority in [
                None,
                Some("Bearer malformed"),
                Some(wrong_token_authority.as_str()),
                Some(nonexistent_authority.as_str()),
                Some("duplicate"),
            ] {
                let mut request = Request::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap();
                if let Some(value) = authority {
                    if value == "duplicate" {
                        request
                            .headers_mut()
                            .append(AUTHORIZATION, format!("Bearer {valid}").parse().unwrap());
                        request
                            .headers_mut()
                            .append(AUTHORIZATION, format!("Bearer {valid}").parse().unwrap());
                    } else {
                        request
                            .headers_mut()
                            .append(AUTHORIZATION, value.parse().unwrap());
                    }
                }
                let (status, _, body) = json_request(app.clone(), request).await;
                assert_eq!(status, StatusCode::NOT_FOUND, "{path} {authority:?}");
                assert_eq!(body["error"]["code"], "not_found");
                assert!(!body.to_string().contains(&valid));
                assert!(!body.to_string().contains(&wrong_token));
                assert!(!body.to_string().contains(valid_token));
            }
        }
        let beta_join = Request::post("/v1/squads/beta/join")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name":"bob","role":"builder","mode":"cooperative","client":{"kind":"test"},"mission":"other"}"#))
            .unwrap();
        let (_, beta_headers, _) = json_request(app.clone(), beta_join).await;
        let beta = beta_headers
            .get("psst-session-credential")
            .unwrap()
            .to_str()
            .unwrap();
        for (path, expected_status, expected_code) in [
            ("/v1/squads/alpha/leave", StatusCode::NOT_FOUND, "not_found"),
            (
                "/v1/squads/alpha/archive",
                StatusCode::FORBIDDEN,
                "not_member",
            ),
        ] {
            let request = Request::post(path)
                .header("authorization", format!("Bearer {beta}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();
            let (status, _, body) = json_request(app.clone(), request).await;
            assert_eq!(status, expected_status);
            assert_eq!(body["error"]["code"], expected_code);
        }
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();

        let (worker, handle) = StoreWorker::start_with_time(
            &path,
            16,
            Duration::from_secs(1),
            Arc::new(FakeTime(UnixMillis::new(40_000).unwrap())),
        )
        .unwrap();
        let app = router(worker.clone());
        for (path, body) in &cases[..3] {
            let request = Request::post(*path)
                .header("authorization", format!("Bearer {valid}"))
                .header("content-type", "application/json")
                .body(Body::from(*body))
                .unwrap();
            let (status, _, body) = json_request(app.clone(), request).await;
            assert_eq!(status, StatusCode::CONFLICT, "{path}");
            assert_eq!(body["error"]["code"], "lease_expired");
        }
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

    async fn raw_get_authorized(address: SocketAddr, path: &str, credential: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {credential}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    async fn raw_post(
        address: SocketAddr,
        path: &str,
        authorization: Option<&str>,
        body: &str,
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let authorization = authorization.map_or_else(String::new, |value| {
            format!("Authorization: Bearer {value}\r\n")
        });
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n{authorization}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    fn response_header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
        response.lines().find_map(|line| {
            let (header, value) = line.split_once(':')?;
            header.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    fn raw_response_json(response: &str) -> serde_json::Value {
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        serde_json::from_str(body).unwrap()
    }

    #[tokio::test]
    async fn bound_clients_atomically_deduplicate_send_and_acknowledgement() {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let directory = tempfile::TempDir::new().unwrap();
        let database = directory.path().join("psst.db");
        let mut config = RelayConfig::local(&database);
        config.bind = address;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(serve(config, shutdown_rx));
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let alice = raw_post(
            address,
            "/v1/squads/alpha/join",
            None,
            r#"{"name":"alice","role":"test","mode":"cooperative","client":{"kind":"test"},"mission":"bound messaging"}"#,
        )
        .await;
        let alice = response_header(&alice, "psst-session-credential")
            .unwrap()
            .to_owned();
        let bob = raw_post(
            address,
            "/v1/squads/alpha/join",
            None,
            r#"{"name":"bob","role":"test","mode":"cooperative","client":{"kind":"test"}}"#,
        )
        .await;
        let bob = response_header(&bob, "psst-session-credential")
            .unwrap()
            .to_owned();
        let send = r#"{"recipient":"bob","body":"once","dedupe_key":"bound-once"}"#;
        let (first, second) = tokio::join!(
            raw_post(address, "/v1/messages", Some(&alice), send),
            raw_post(address, "/v1/messages", Some(&alice), send)
        );
        assert!(first.starts_with("HTTP/1.1 200"));
        assert!(second.starts_with("HTTP/1.1 200"));
        let first = raw_response_json(&first);
        let second = raw_response_json(&second);
        assert_eq!(first["message"]["id"], second["message"]["id"]);
        assert_ne!(first["idempotent_replay"], second["idempotent_replay"]);
        let id = first["message"]["id"].as_str().unwrap();
        let ack = format!(r#"{{"message_ids":["{id}"]}}"#);
        let (first, second) = tokio::join!(
            raw_post(address, "/v1/messages/ack", Some(&bob), &ack),
            raw_post(address, "/v1/messages/ack", Some(&bob), &ack)
        );
        assert!(first.starts_with("HTTP/1.1 200"));
        assert!(second.starts_with("HTTP/1.1 200"));
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
        let connection = rusqlite::Connection::open(database).unwrap();
        let (rows, acknowledged): (u64, u64) = connection
            .query_row(
                "SELECT COUNT(*), COUNT(acknowledged_at) FROM messages",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((rows, acknowledged), (1, 1));
    }

    #[tokio::test]
    async fn bound_http_clients_serialize_name_claim_and_archive_leave_race() {
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
        assert!(connected, "relay did not bind before raw clients started");
        let body = r#"{"name":"same","role":"builder","mode":"cooperative","client":{"kind":"test"},"mission":"race"}"#;
        let (first, second) = tokio::join!(
            raw_post(address, "/v1/squads/alpha/join", None, body),
            raw_post(address, "/v1/squads/alpha/join", None, body)
        );
        let statuses = [
            first.starts_with("HTTP/1.1 200"),
            second.starts_with("HTTP/1.1 200"),
        ];
        assert_eq!(statuses.into_iter().filter(|success| *success).count(), 1);
        let conflict = if statuses[0] { &second } else { &first };
        assert!(conflict.starts_with("HTTP/1.1 409"));
        assert!(conflict.contains("name_in_use"));
        let successful = if statuses[0] { &first } else { &second };
        let credential = response_header(successful, "psst-session-credential").unwrap();

        let (archive, leave) = tokio::join!(
            raw_post(address, "/v1/squads/alpha/archive", Some(credential), "{}"),
            raw_post(address, "/v1/squads/alpha/leave", Some(credential), "{}")
        );
        let successes = [archive.as_str(), leave.as_str()]
            .into_iter()
            .filter(|response| response.starts_with("HTTP/1.1 200"))
            .count();
        assert_eq!(successes, 1);
        let loser = if archive.starts_with("HTTP/1.1 200") {
            &leave
        } else {
            &archive
        };
        assert!(loser.starts_with("HTTP/1.1 403") || loser.starts_with("HTTP/1.1 409"));
        assert!(loser.contains("not_member") || loser.contains("squad_archived"));
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
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
            None,
            move |worker, config, shutdown| {
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
                        .with_state(AppState { worker, shutdown }),
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

    async fn join_for_messaging(app: Router, squad: &str, name: &str, mission: bool) -> String {
        let mission = if mission {
            r#", "mission":"message tests""#
        } else {
            ""
        };
        let body = format!(
            r#"{{"name":"{name}","role":"tester","mode":"cooperative","client":{{"kind":"test"}}{mission}}}"#
        );
        let (status, headers, _) = json_request(
            app,
            Request::post(format!("/v1/squads/{squad}/join"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        headers
            .get("psst-session-credential")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn long_poll_wakes_all_relevant_watchers_and_cleans_registrations() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start(&directory.path().join("psst.db"), 256).unwrap();
        let app = router_with_limits(worker.clone(), 512 * 1024, 128, Duration::from_secs(5));
        let alice = join_for_messaging(app.clone(), "alpha", "alice", true).await;
        let bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;

        let mut watchers = Vec::new();
        for _ in 0..100 {
            let app = app.clone();
            let bob = bob.clone();
            watchers.push(tokio::spawn(async move {
                json_request(
                    app,
                    Request::get("/v1/inbox?limit=100&wait=2")
                        .header("authorization", format!("Bearer {bob}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
            }));
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while worker.inner.notifications.registration_count() != 100 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let (status, _, _) = json_request(
            app,
            Request::post("/v1/messages")
                .header("authorization", format!("Bearer {alice}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"recipient":"bob","body":"wake","priority":"normal","dedupe_key":"wake-1"}"#,
                ))
                .unwrap(),
        ).await;
        assert_eq!(status, StatusCode::OK);
        for watcher in watchers {
            let (status, _, inbox) = watcher.await.unwrap();
            assert_eq!(status, StatusCode::OK);
            assert_eq!(inbox["pending_count"], 1);
        }
        assert_eq!(worker.inner.notifications.registration_count(), 0);
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn long_poll_timeout_is_empty_and_does_not_acknowledge_future_mail() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start(&directory.path().join("psst.db"), 16).unwrap();
        let app = router(worker.clone());
        let _alice = join_for_messaging(app.clone(), "alpha", "alice", true).await;
        let bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;
        let started = tokio::time::Instant::now();
        let (status, _, inbox) = json_request(
            app,
            Request::get("/v1/inbox?limit=100&wait=1")
                .header("authorization", format!("Bearer {bob}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(started.elapsed() >= Duration::from_millis(900));
        assert_eq!(inbox["pending_count"], 0);
        assert!(inbox["messages"].as_array().unwrap().is_empty());
        assert_eq!(worker.inner.notifications.registration_count(), 0);
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancellation_irrelevant_send_and_shutdown_release_waiters() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start(&directory.path().join("psst.db"), 32).unwrap();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let app = router_with_limits_and_shutdown(
            worker.clone(),
            512 * 1024,
            128,
            Duration::from_secs(5),
            shutdown_rx,
        );
        let alice = join_for_messaging(app.clone(), "alpha", "alice", true).await;
        let bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;
        let charlie = join_for_messaging(app.clone(), "alpha", "charlie", false).await;
        let wait = |credential: String| {
            let app = app.clone();
            tokio::spawn(async move {
                json_request(
                    app,
                    Request::get("/v1/inbox?limit=100&wait=30")
                        .header("authorization", format!("Bearer {credential}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
            })
        };
        let bob_wait = wait(bob.clone());
        let charlie_wait = wait(charlie);
        tokio::time::timeout(Duration::from_secs(2), async {
            while worker.inner.notifications.registration_count() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let (status, _, _) = json_request(
            app.clone(),
            Request::post("/v1/messages")
                .header("authorization", format!("Bearer {alice}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"recipient":"bob","body":"only bob","priority":"normal","dedupe_key":"isolation-1"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let bob_inbox = bob_wait.await.unwrap().2;
        assert_eq!(bob_inbox["pending_count"], 1);
        assert_eq!(worker.inner.notifications.registration_count(), 1);

        charlie_wait.abort();
        assert!(charlie_wait.await.unwrap_err().is_cancelled());
        assert_eq!(worker.inner.notifications.registration_count(), 0);

        let message_id = bob_inbox["messages"][0]["id"].as_str().unwrap();
        let (status, _, _) = json_request(
            app.clone(),
            Request::post("/v1/messages/ack")
                .header("authorization", format!("Bearer {bob}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"message_ids":["{message_id}"]}}"#)))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let shutdown_wait = wait(bob);
        tokio::time::timeout(Duration::from_secs(2), async {
            while worker.inner.notifications.registration_count() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        shutdown_tx.send(true).unwrap();
        let (status, _, body) = shutdown_wait.await.unwrap();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "database_busy");
        assert_eq!(worker.inner.notifications.registration_count(), 0);
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn waiter_capacity_is_stable_retryable_and_preexisting_mail_never_sleeps() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start(&directory.path().join("psst.db"), 256).unwrap();
        let app = router_with_limits(worker.clone(), 512 * 1024, 256, Duration::from_secs(5));
        let alice = join_for_messaging(app.clone(), "alpha", "alice", true).await;
        let bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;
        let (status, _, _) = json_request(
            app.clone(),
            Request::post("/v1/messages")
                .header("authorization", format!("Bearer {alice}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"recipient":"bob","body":"already here","priority":"normal","dedupe_key":"preexisting-1"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let started = tokio::time::Instant::now();
        let (_, _, inbox) = json_request(
            app.clone(),
            Request::get("/v1/inbox?limit=100&wait=30")
                .header("authorization", format!("Bearer {bob}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(inbox["pending_count"], 1);
        assert!(started.elapsed() < Duration::from_secs(1));

        // Use a fresh recipient so every request reaches the wait registry.
        let charlie = join_for_messaging(app.clone(), "alpha", "charlie", false).await;
        let mut waits = Vec::new();
        for _ in 0..MAX_INBOX_WAITERS {
            let app = app.clone();
            let charlie = charlie.clone();
            waits.push(tokio::spawn(async move {
                json_request(
                    app,
                    Request::get("/v1/inbox?limit=1&wait=30")
                        .header("authorization", format!("Bearer {charlie}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
            }));
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while worker.inner.notifications.registration_count() != MAX_INBOX_WAITERS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let (status, _, body) = json_request(
            app,
            Request::get("/v1/inbox?limit=1&wait=30")
                .header("authorization", format!("Bearer {charlie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["code"], "rate_limited");
        for wait in waits {
            wait.abort();
            assert!(wait.await.unwrap_err().is_cancelled());
        }
        assert_eq!(worker.inner.notifications.registration_count(), 0);
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn committed_send_in_preflight_subscription_gap_is_reconciled() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start(&directory.path().join("psst.db"), 32).unwrap();
        let app = router(worker.clone());
        let alice = join_for_messaging(app.clone(), "alpha", "alice", true).await;
        let bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;
        let (preflight_done, release) = worker.pause_next_inbox_after_preflight();
        let waiting = tokio::spawn(json_request(
            app.clone(),
            Request::get("/v1/inbox?limit=100&wait=30")
                .header("authorization", format!("Bearer {bob}"))
                .body(Body::empty())
                .unwrap(),
        ));
        preflight_done.await.unwrap();
        let response = app.oneshot(
            Request::post("/v1/messages")
                .header("authorization", format!("Bearer {alice}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"recipient":"bob","body":"in the gap","priority":"normal","dedupe_key":"gap-1"}"#))
                .unwrap(),
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        release.send(()).unwrap();
        let (status, _, inbox) = waiting.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(inbox["pending_count"], 1);
        assert_eq!(inbox["messages"][0]["body"], "in the gap");
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn committed_send_wakes_waiter_even_when_sender_http_deadline_expires() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start_with_time(
            &directory.path().join("psst.db"),
            32,
            Duration::from_secs(2),
            Arc::new(SystemTimeSource),
        )
        .unwrap();
        // Leave enough headroom for a contended CI runner to register the waiter and
        // dispatch the send before the shared HTTP deadline. The delayed reply remains
        // well beyond that deadline, so the behavior under test is unchanged.
        let app = router_with_limits(worker.clone(), 512 * 1024, 128, Duration::from_millis(250));
        let alice = join_for_messaging(app.clone(), "alpha", "alice", true).await;
        let bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;
        let waiting = tokio::spawn(json_request(
            app.clone(),
            Request::get("/v1/inbox?limit=100&wait=30")
                .header("authorization", format!("Bearer {bob}"))
                .body(Body::empty())
                .unwrap(),
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            while worker.inner.notifications.registration_count() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let completion = worker
            .delay_next_send_reply(Duration::from_secs(1))
            .await
            .unwrap();
        let response = app.oneshot(
            Request::post("/v1/messages")
                .header("authorization", format!("Bearer {alice}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"recipient":"bob","body":"committed","priority":"normal","dedupe_key":"timeout-wake-1"}"#)).unwrap(),
            ).await.unwrap();
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        let (_, _, inbox) = waiting.await.unwrap();
        assert_eq!(inbox["pending_count"], 1);
        completion.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn failed_send_does_not_advance_recipient_or_wake_waiter() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start(&directory.path().join("psst.db"), 32).unwrap();
        let app = router(worker.clone());
        let alice = join_for_messaging(app.clone(), "alpha", "alice", true).await;
        let bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;
        let waiting = tokio::spawn(json_request(
            app.clone(),
            Request::get("/v1/inbox?limit=100&wait=1")
                .header("authorization", format!("Bearer {bob}"))
                .body(Body::empty())
                .unwrap(),
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            while worker.inner.notifications.registration_count() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let (status, _, _) = json_request(
            app,
            Request::post("/v1/messages")
                .header("authorization", format!("Bearer {alice}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"recipient":"missing","body":"must fail","priority":"normal","dedupe_key":"failed-1"}"#)).unwrap(),
        ).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            !waiting.is_finished(),
            "failed send spuriously woke the waiter"
        );
        assert_eq!(worker.inner.notifications.registration_count(), 1);
        let (status, _, inbox) = waiting.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(inbox["pending_count"], 0);
        assert_eq!(worker.inner.notifications.registration_count(), 0);
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn inbox_deadline_allowance_requires_one_valid_canonical_wait() {
        let allowance =
            |value: &str| inbox_wait_allowance(&value.parse::<axum::http::Uri>().unwrap());
        assert_eq!(allowance("/v1/inbox?limit=1&wait=0"), Some(Duration::ZERO));
        assert_eq!(
            allowance("/v1/inbox?limit=1&wait=30"),
            Some(Duration::from_secs(30))
        );
        for invalid in [
            "/v1/inbox?limit=1",
            "/v1/inbox?wait=1&wait=2",
            "/v1/inbox?wait=31",
            "/v1/inbox?wait=overflow",
            "/v1/inbox?wait=",
            "/v1/messages?wait=1",
        ] {
            assert_eq!(allowance(invalid), None, "{invalid}");
        }
    }

    #[tokio::test]
    async fn inbox_deadline_is_exact_base_plus_valid_wait_allowance() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start_with_time(
            &directory.path().join("psst.db"),
            8,
            Duration::from_secs(2),
            Arc::new(SystemTimeSource),
        )
        .unwrap();
        // Session setup is not part of this deadline assertion and must not inherit
        // its deliberately tiny timeout on a contended CI runner.
        let setup_app = router(worker.clone());
        let _alice = join_for_messaging(setup_app.clone(), "alpha", "alice", true).await;
        let bob = join_for_messaging(setup_app, "alpha", "bob", false).await;
        let app = router_with_limits(worker.clone(), 512 * 1024, 8, Duration::from_millis(25));

        for (wait, maximum) in [
            (0, Duration::from_millis(200)),
            (1, Duration::from_millis(1_200)),
        ] {
            let (started_tx, started_rx) = oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocked_worker = worker.clone();
            let block_task = tokio::spawn(async move {
                blocked_worker
                    .controlled_block(started_tx, release_rx)
                    .await
            });
            started_rx.await.unwrap();
            let response = tokio::time::timeout(
                maximum,
                app.clone().oneshot(
                    Request::get(format!("/v1/inbox?limit=1&wait={wait}"))
                        .header("authorization", format!("Bearer {bob}"))
                        .body(Body::empty())
                        .unwrap(),
                ),
            )
            .await
            .expect("middleware deadline exceeded its exact allowance")
            .unwrap();
            assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
            release_tx.send(()).unwrap();
            block_task.await.unwrap().unwrap();
        }
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn messaging_http_is_durable_replayable_bounded_and_path_authorized() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let clock = Arc::new(FakeTime(UnixMillis::new(100).unwrap()));
        let (worker, handle) =
            StoreWorker::start_with_time(&path, 32, Duration::from_secs(2), clock.clone()).unwrap();
        let app = router(worker.clone());
        let alice = join_for_messaging(app.clone(), "alpha", "alice", true).await;
        let bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;
        let mallory = join_for_messaging(app.clone(), "beta", "mallory", true).await;

        let send_body = r#"{"recipient":"bob","body":"hello","priority":"high","dedupe_key":"logical-1","correlation_id":"thread-1"}"#;
        let send_request = || {
            Request::post("/v1/messages")
                .header("authorization", format!("Bearer {alice}"))
                .header("content-type", "application/json")
                .body(Body::from(send_body))
                .unwrap()
        };
        let first = tokio::spawn(json_request(app.clone(), send_request()));
        let second = tokio::spawn(json_request(app.clone(), send_request()));
        let (_, _, first) = first.await.unwrap();
        let (_, _, second) = second.await.unwrap();
        assert_eq!(first["message"]["id"], second["message"]["id"]);
        assert_ne!(first["idempotent_replay"], second["idempotent_replay"]);
        let message_id = first["message"]["id"].as_str().unwrap().to_owned();
        assert_eq!(first["message"]["sender"], "alice");
        assert_eq!(first["message"]["recipient"], "bob");

        for _ in 0..2 {
            let (status, _, inbox) = json_request(
                app.clone(),
                Request::get("/v1/inbox?limit=1&wait=0")
                    .header("authorization", format!("Bearer {bob}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(inbox["pending_count"], 1);
            assert_eq!(inbox["messages"][0]["id"], message_id);
            assert!(inbox["messages"][0]["acknowledged_at"].is_null());
        }

        let (status, _, wrong_squad) = json_request(
            app.clone(),
            Request::get("/v1/squads/alpha/transcript?after=0&limit=100")
                .header("authorization", format!("Bearer {mallory}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(wrong_squad["error"]["code"], "not_member");

        let ack_body =
            format!(r#"{{"message_ids":["{message_id}","msg_unknown","{message_id}"]}}"#);
        let (status, _, failed_ack) = json_request(
            app.clone(),
            Request::post("/v1/messages/ack")
                .header("authorization", format!("Bearer {bob}"))
                .header("content-type", "application/json")
                .body(Body::from(ack_body))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(failed_ack["error"]["code"], "not_found");
        let ack_body = format!(r#"{{"message_ids":["{message_id}","{message_id}"]}}"#);
        let (status, _, acked) = json_request(
            app.clone(),
            Request::post("/v1/messages/ack")
                .header("authorization", format!("Bearer {bob}"))
                .header("content-type", "application/json")
                .body(Body::from(ack_body))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(acked["acknowledged_ids"].as_array().unwrap().len(), 1);
        assert!(acked.get("acknowledged_at").is_none());

        let (status, _, transcript) = json_request(
            app.clone(),
            Request::get("/v1/squads/alpha/transcript?after=0&limit=1")
                .header("authorization", format!("Bearer {alice}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(transcript["messages"][0]["id"], message_id);
        assert!(transcript["messages"][0]["acknowledged_at"].is_string());
        let cursor = transcript["next_after"].as_i64().unwrap();
        let (status, _, empty) = json_request(
            app.clone(),
            Request::get(format!(
                "/v1/squads/alpha/transcript?after={cursor}&limit=1"
            ))
            .header("authorization", format!("Bearer {alice}"))
            .body(Body::empty())
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(empty["messages"].as_array().unwrap().is_empty());

        for uri in [
            "/v1/inbox?limit=0&wait=0",
            "/v1/inbox?limit=1&wait=31",
            "/v1/inbox?limit=overflow&wait=0",
            "/v1/squads/alpha/transcript?after=-1&limit=1",
            "/v1/squads/alpha/transcript?after=0&limit=101",
        ] {
            let (status, _, body) = json_request(
                app.clone(),
                Request::get(uri)
                    .header("authorization", format!("Bearer {alice}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["error"]["code"], "invalid_request");
        }

        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
        let (worker, handle) =
            StoreWorker::start_with_time(&path, 16, Duration::from_secs(2), clock).unwrap();
        let (status, _, transcript) = json_request(
            router(worker.clone()),
            Request::get("/v1/squads/alpha/transcript?after=0&limit=100")
                .header("authorization", format!("Bearer {alice}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(transcript["messages"].as_array().unwrap().len(), 1);
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn maximum_utf8_body_survives_json_escaping_and_default_body_limit() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start(&directory.path().join("psst.db"), 8).unwrap();
        let app = router(worker.clone());
        let alice = join_for_messaging(app.clone(), "alpha", "alice", true).await;
        let _bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;
        let request_body = serde_json::to_vec(&serde_json::json!({
            "recipient": "bob",
            "body": "\u{0001}".repeat(MessageBody::MAX_BYTES),
            "dedupe_key": "body-bound"
        }))
        .unwrap();
        assert!(request_body.len() > 128 * 1024);
        let response = app
            .oneshot(
                Request::post("/v1/messages")
                    .header("authorization", format!("Bearer {alice}"))
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["message"]["body"].as_str().unwrap().len(),
            MessageBody::MAX_BYTES
        );
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn http_retry_after_committed_reply_timeout_returns_the_one_durable_message() {
        let directory = tempfile::TempDir::new().unwrap();
        let database = directory.path().join("psst.db");
        let now = UnixMillis::new(1_234).unwrap();
        let (worker, handle) = StoreWorker::start_with_time(
            &database,
            8,
            Duration::from_millis(500),
            Arc::new(FakeTime(now)),
        )
        .unwrap();
        let app = router(worker.clone());
        let alice = join_for_messaging(app.clone(), "alpha", "alice", true).await;
        let _bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;
        let send_completed = worker
            .delay_next_send_reply(Duration::from_millis(750))
            .await
            .unwrap();
        let request = || {
            Request::post("/v1/messages")
                .header("authorization", format!("Bearer {alice}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"recipient":"bob","body":"committed","dedupe_key":"ambiguous-http"}"#,
                ))
                .unwrap()
        };
        let (status, _, timed_out) = json_request(app.clone(), request()).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(timed_out["error"]["code"], "database_busy");
        send_completed.recv_timeout(Duration::from_secs(2)).unwrap();
        let (status, _, replay) = json_request(app, request()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replay["idempotent_replay"], true);
        assert_eq!(replay["message"]["sequence"], 1);
        assert_eq!(replay["message"]["created_at"], "1970-01-01T00:00:01.234Z");
        let connection = rusqlite::Connection::open(&database).unwrap();
        let (count, id, sequence, created_at): (u64, String, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), id, sequence, created_at FROM messages",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(replay["message"]["id"], id);
        assert_eq!(replay["message"]["sequence"], sequence);
        assert_eq!(created_at, now.as_i64());
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn exact_http_retry_survives_recipient_leave_and_name_reuse_without_retargeting() {
        let directory = tempfile::TempDir::new().unwrap();
        let database = directory.path().join("psst.db");
        let (worker, handle) = StoreWorker::start(&database, 16).unwrap();
        let app = router(worker.clone());
        let alice = join_for_messaging(app.clone(), "alpha", "alice", true).await;
        let bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;
        let request_body = r#"{"recipient":"bob","body":"original","dedupe_key":"name-reuse"}"#;
        let request = || {
            Request::post("/v1/messages")
                .header("authorization", format!("Bearer {alice}"))
                .header("content-type", "application/json")
                .body(Body::from(request_body))
                .unwrap()
        };
        let (status, _, original) = json_request(app.clone(), request()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(original["idempotent_replay"], false);
        let original_id = original["message"]["id"].as_str().unwrap().to_owned();
        let original_recipient: String = rusqlite::Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT recipient_membership_id FROM messages WHERE id=?1",
                [&original_id],
                |row| row.get(0),
            )
            .unwrap();
        let (status, _, _) = json_request(
            app.clone(),
            Request::post("/v1/squads/alpha/leave")
                .header("authorization", format!("Bearer {bob}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let _replacement_bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;
        let active_recipient: String = rusqlite::Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT id FROM memberships WHERE normalized_name='bob' AND left_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_ne!(active_recipient, original_recipient);

        let (status, _, replay) = json_request(app.clone(), request()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replay["idempotent_replay"], true);
        assert_eq!(replay["message"]["id"], original_id);
        let (rows, durable_recipient): (u64, String) = rusqlite::Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT COUNT(*), recipient_membership_id FROM messages",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(durable_recipient, original_recipient);
        assert_ne!(durable_recipient, active_recipient);

        let (status, _, conflict) = json_request(
            app,
            Request::post("/v1/messages")
                .header("authorization", format!("Bearer {alice}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"recipient":"bob","body":"changed","dedupe_key":"name-reuse"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(conflict["error"]["code"], "idempotency_conflict");
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn real_http_offline_restart_replay_ack_restart_journey_is_durable() {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let directory = tempfile::TempDir::new().unwrap();
        let database = directory.path().join("psst.db");
        let start = |shutdown_rx| {
            let mut config = RelayConfig::local(&database);
            config.bind = address;
            tokio::spawn(serve(config, shutdown_rx))
        };
        let wait_ready = || async {
            for _ in 0..100 {
                if tokio::net::TcpStream::connect(address).await.is_ok() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("relay did not bind");
        };
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = start(shutdown_rx);
        wait_ready().await;
        let alice = raw_post(
            address,
            "/v1/squads/alpha/join",
            None,
            r#"{"name":"alice","role":"test","mode":"cooperative","client":{"kind":"test"},"mission":"restart journey"}"#,
        )
        .await;
        let alice = response_header(&alice, "psst-session-credential")
            .unwrap()
            .to_owned();
        let bob = raw_post(
            address,
            "/v1/squads/alpha/join",
            None,
            r#"{"name":"bob","role":"test","mode":"cooperative","client":{"kind":"test"}}"#,
        )
        .await;
        let bob = response_header(&bob, "psst-session-credential")
            .unwrap()
            .to_owned();
        let sent = raw_post(
            address,
            "/v1/messages",
            Some(&alice),
            r#"{"recipient":"bob","body":"offline durable","dedupe_key":"restart-mail"}"#,
        )
        .await;
        assert!(sent.starts_with("HTTP/1.1 200"));
        let message_id = raw_response_json(&sent)["message"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = start(shutdown_rx);
        wait_ready().await;
        for _ in 0..2 {
            let inbox = raw_get_authorized(address, "/v1/inbox?limit=100&wait=0", &bob).await;
            assert!(inbox.starts_with("HTTP/1.1 200"));
            let inbox = raw_response_json(&inbox);
            assert_eq!(inbox["pending_count"], 1);
            assert_eq!(inbox["messages"][0]["id"], message_id);
        }
        let ack = raw_post(
            address,
            "/v1/messages/ack",
            Some(&bob),
            &format!(r#"{{"message_ids":["{message_id}"]}}"#),
        )
        .await;
        assert!(ack.starts_with("HTTP/1.1 200"));
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = start(shutdown_rx);
        wait_ready().await;
        let inbox = raw_get_authorized(address, "/v1/inbox?limit=100&wait=0", &bob).await;
        let inbox = raw_response_json(&inbox);
        assert_eq!(inbox["pending_count"], 0);
        assert!(inbox["messages"].as_array().unwrap().is_empty());
        let transcript = raw_get_authorized(
            address,
            "/v1/squads/alpha/transcript?after=0&limit=100",
            &alice,
        )
        .await;
        let transcript = raw_response_json(&transcript);
        assert_eq!(transcript["messages"][0]["id"], message_id);
        assert!(transcript["messages"][0]["acknowledged_at"].is_string());
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn projected_http_inbox_truncates_under_one_mib_and_preserves_omitted_mail() {
        let directory = tempfile::TempDir::new().unwrap();
        let (worker, handle) = StoreWorker::start(&directory.path().join("psst.db"), 32).unwrap();
        let app = router(worker.clone());
        let alice = join_for_messaging(app.clone(), "alpha", "alice", true).await;
        let bob = join_for_messaging(app.clone(), "alpha", "bob", false).await;
        let large_body = "\u{0001}".repeat(MessageBody::MAX_BYTES);
        for index in 0..4 {
            let body = serde_json::to_vec(&serde_json::json!({
                "recipient": "bob",
                "body": large_body,
                "dedupe_key": format!("large-{index}")
            }))
            .unwrap();
            let response = app
                .clone()
                .oneshot(
                    Request::post("/v1/messages")
                        .header("authorization", format!("Bearer {alice}"))
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        let response = app
            .clone()
            .oneshot(
                Request::get("/v1/inbox?limit=100&wait=0")
                    .header("authorization", format!("Bearer {bob}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let encoded = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        assert!(encoded.len() <= psst_protocol::MAX_INBOX_BYTES);
        let first: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(first["pending_count"], 4);
        let returned = first["messages"].as_array().unwrap();
        assert!(!returned.is_empty());
        assert!(returned.len() < 4);
        let first_ids = returned
            .iter()
            .map(|message| message["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let ack = serde_json::to_vec(&serde_json::json!({"message_ids": first_ids})).unwrap();
        let (status, _, _) = json_request(
            app.clone(),
            Request::post("/v1/messages/ack")
                .header("authorization", format!("Bearer {bob}"))
                .header("content-type", "application/json")
                .body(Body::from(ack))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let response = app
            .oneshot(
                Request::get("/v1/inbox?limit=100&wait=0")
                    .header("authorization", format!("Bearer {bob}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let encoded = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        assert!(encoded.len() <= psst_protocol::MAX_INBOX_BYTES);
        let second: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            second["pending_count"],
            4_u64 - u64::try_from(first_ids.len()).unwrap()
        );
        assert!(!second["messages"].as_array().unwrap().is_empty());
        assert!(
            second["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|message| {
                    !first_ids
                        .iter()
                        .any(|id| id == message["id"].as_str().unwrap())
                })
        );
        worker.begin_shutdown();
        handle.join().unwrap().unwrap();
    }
}
