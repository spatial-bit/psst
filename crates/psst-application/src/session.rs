use crate::leave_journal::{LeaveJournal, LeaveJournalStore, LeavePhase};
use crate::{ProfileBinding, ProfileLock, ProfilePaths, clear_profile, store_profile};
use psst_client::{
    Client, Credential, CredentialBinding, CredentialStore, Error as ClientError, PreparedSend,
    PreparedSendIdentity,
};
use psst_protocol::{
    AckMessagesRequest, AckMessagesResponse, AgentModeDto, ApiErrorCode, ApiTimestamp,
    ArchiveSquadResponse, AvailabilityDto, AvailabilitySourceDto, ClientMetadata, HeartbeatRequest,
    InboxResponse, JoinSquadRequest, LeaveSquadResponse, MessageSequence, ResumeSquadRequest,
    RosterResponse, SendMessageResponse, SessionResponse, TranscriptResponse,
};
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Notify, RwLock},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionHealth {
    Ready,
    Degraded,
    OutcomeUnknown,
    RotationFailed,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSnapshot {
    pub health: SessionHealth,
    pub generation: u64,
    pub instance_id: String,
    pub heartbeat_interval_seconds: u32,
    pub lease_expires_at: ApiTimestamp,
    pub availability: AvailabilityDto,
    pub availability_source: AvailabilitySourceDto,
}

#[derive(Debug)]
pub enum SessionError {
    Local(io::Error),
    Relay(ClientError),
    ShutdownTimedOut,
    NotReady,
    Unbound,
    RecoveryOutcomeUnknown,
    SendCapacity,
    OperationCapacity,
}
impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(_) => f.write_str("local session state failed"),
            Self::Relay(_) => f.write_str("relay session validation failed"),
            Self::ShutdownTimedOut => f.write_str("session shutdown timed out"),
            Self::NotReady => f.write_str("session is not ready for protected operations"),
            Self::Unbound => f.write_str("session profile is no longer bound"),
            Self::RecoveryOutcomeUnknown => {
                f.write_str("unbound session has an unresolved leave intent")
            }
            Self::SendCapacity => f.write_str("session send capacity is busy"),
            Self::OperationCapacity => f.write_str("session operation capacity is busy"),
        }
    }
}
impl std::error::Error for SessionError {}
impl From<io::Error> for SessionError {
    fn from(value: io::Error) -> Self {
        Self::Local(value)
    }
}

pub struct RuntimeSpec {
    pub profile: ProfileBinding,
    pub paths: ProfilePaths,
    pub mode: AgentModeDto,
    pub client_metadata: ClientMetadata,
    pub shutdown_bound: Duration,
}
pub struct UnboundRuntimeSpec {
    pub relay_origin: String,
    pub profile_name: String,
    pub squad: String,
    pub paths: ProfilePaths,
    pub shutdown_bound: Duration,
}
pub struct JoinedRuntime {
    pub runtime: SessionRuntime,
    pub response: SessionResponse,
}

struct Shared {
    credential: Arc<Credential>,
    instance_id: String,
    heartbeat_interval_seconds: u32,
    lease_expires_at: ApiTimestamp,
    availability: AvailabilityDto,
    availability_source: AvailabilitySourceDto,
    health: SessionHealth,
    generation: u64,
}

type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ClientError>> + Send + 'a>>;

trait SessionTransport: Send + Sync {
    fn join<'a>(
        &'a self,
        squad: &'a str,
        request: &'a JoinSquadRequest,
    ) -> TransportFuture<'a, psst_client::Session>;
    fn leave<'a>(
        &'a self,
        squad: &'a str,
        credential: &'a Credential,
    ) -> TransportFuture<'a, LeaveSquadResponse>;
    fn heartbeat<'a>(
        &'a self,
        request: &'a HeartbeatRequest,
        credential: &'a Credential,
    ) -> TransportFuture<'a, psst_protocol::HeartbeatResponse>;
    fn resume<'a>(
        &'a self,
        squad: &'a str,
        request: &'a ResumeSquadRequest,
        credential: &'a Credential,
    ) -> TransportFuture<'a, psst_client::Session>;
    fn send_prepared<'a>(
        &'a self,
        _request: &'a PreparedSend,
        _credential: &'a Credential,
    ) -> TransportFuture<'a, SendMessageResponse> {
        Box::pin(async { Err(ClientError::InvalidConfiguration) })
    }
}

impl SessionTransport for Client {
    fn join<'a>(
        &'a self,
        squad: &'a str,
        request: &'a JoinSquadRequest,
    ) -> TransportFuture<'a, psst_client::Session> {
        Box::pin(self.join(squad, request))
    }
    fn leave<'a>(
        &'a self,
        squad: &'a str,
        credential: &'a Credential,
    ) -> TransportFuture<'a, LeaveSquadResponse> {
        Box::pin(self.leave(squad, credential))
    }
    fn heartbeat<'a>(
        &'a self,
        request: &'a HeartbeatRequest,
        credential: &'a Credential,
    ) -> TransportFuture<'a, psst_protocol::HeartbeatResponse> {
        Box::pin(self.heartbeat(request, credential))
    }
    fn resume<'a>(
        &'a self,
        squad: &'a str,
        request: &'a ResumeSquadRequest,
        credential: &'a Credential,
    ) -> TransportFuture<'a, psst_client::Session> {
        Box::pin(self.resume(squad, request, credential))
    }
    fn send_prepared<'a>(
        &'a self,
        request: &'a PreparedSend,
        credential: &'a Credential,
    ) -> TransportFuture<'a, SendMessageResponse> {
        Box::pin(self.send_prepared(request, credential))
    }
}

trait RotationStore: Send + Sync {
    fn persist(&self, binding: &CredentialBinding, credential: &Credential) -> io::Result<()>;
}
trait SessionClock: Send + Sync {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

trait JournalIssuer: Send + Sync {
    fn intent(&self, binding: &ProfileBinding) -> io::Result<LeaveJournal>;
    fn confirm(&self, intent: &LeaveJournal) -> io::Result<LeaveJournal>;
}

struct SystemJournalIssuer;

impl SystemJournalIssuer {
    fn now() -> io::Result<ApiTimestamp> {
        let now = time::OffsetDateTime::now_utc();
        let normalized = now
            .replace_nanosecond((now.nanosecond() / 1_000_000) * 1_000_000)
            .map_err(|_| io::Error::other("system time cannot be normalized"))?;
        ApiTimestamp::new(normalized)
            .map_err(|_| io::Error::other("system time cannot be represented"))
    }
}

impl JournalIssuer for SystemJournalIssuer {
    fn intent(&self, binding: &ProfileBinding) -> io::Result<LeaveJournal> {
        let mut random = [0_u8; 16];
        psst_platform_security::fill_secure_random(&mut random)?;
        let operation_id = random
            .iter()
            .fold(String::with_capacity(32), |mut value, byte| {
                use std::fmt::Write as _;
                write!(value, "{byte:02x}").expect("writing to a String cannot fail");
                value
            });
        LeaveJournal::intent(binding, operation_id, Self::now()?)
    }

    fn confirm(&self, intent: &LeaveJournal) -> io::Result<LeaveJournal> {
        intent.confirmed(Self::now()?)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupStep {
    Credential,
    Profile,
    Journal,
}

trait CleanupSeam: Send + Sync {
    fn before(&self, _step: CleanupStep) -> io::Result<()> {
        Ok(())
    }
}
struct NoCleanupFault;
impl CleanupSeam for NoCleanupFault {}
struct TokioClock;
impl SessionClock for TokioClock {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

enum ResumeTransactionError {
    Relay(ClientError),
    Persist(io::Error),
    Task,
}

fn resume_session_error(error: ResumeTransactionError) -> SessionError {
    match error {
        ResumeTransactionError::Relay(error) => SessionError::Relay(error),
        ResumeTransactionError::Persist(error) => SessionError::Local(error),
        ResumeTransactionError::Task => {
            SessionError::Local(io::Error::other("resume transaction task failed"))
        }
    }
}

async fn publish_resume_failure(
    supervisor: &SupervisorState,
    error: ResumeTransactionError,
) -> SessionError {
    let (lifecycle, health, error) = match error {
        ResumeTransactionError::Relay(ClientError::OutcomeUnknown) => (
            LifecycleState::OutcomeUnknown,
            SessionHealth::OutcomeUnknown,
            SessionError::Relay(ClientError::OutcomeUnknown),
        ),
        ResumeTransactionError::Relay(error) => (
            LifecycleState::Ready,
            SessionHealth::Degraded,
            SessionError::Relay(error),
        ),
        ResumeTransactionError::Persist(error) => (
            LifecycleState::RotationFailed,
            SessionHealth::RotationFailed,
            SessionError::Local(error),
        ),
        ResumeTransactionError::Task => (
            LifecycleState::Ready,
            SessionHealth::Degraded,
            SessionError::Local(io::Error::other("resume transaction task failed")),
        ),
    };
    let mut current = supervisor.lifecycle.write().await;
    if *current != LifecycleState::Recovering {
        return error;
    }
    supervisor.shared.write().await.health = health;
    *current = lifecycle;
    error
}

async fn resume_transaction(
    transport: Arc<dyn SessionTransport>,
    store: Arc<dyn RotationStore>,
    binding: Arc<CredentialBinding>,
    ownership: Arc<ProfileLock>,
    squad: String,
    request: ResumeSquadRequest,
    credential: Arc<Credential>,
) -> Result<psst_client::Session, ResumeTransactionError> {
    tokio::spawn(async move {
        let _ownership = ownership;
        let session = transport
            .resume(&squad, &request, &credential)
            .await
            .map_err(ResumeTransactionError::Relay)?;
        if session.response.squad.id != binding.squad_id()
            || session.response.squad.name != squad
            || session.response.membership_id != binding.member_id()
        {
            return Err(ResumeTransactionError::Relay(
                ClientError::MalformedResponse { status: 200 },
            ));
        }
        store
            .persist(&binding, &session.credential)
            .map_err(ResumeTransactionError::Persist)?;
        Ok(session)
    })
    .await
    .map_err(|_| ResumeTransactionError::Task)?
}

impl RotationStore for CredentialStore {
    fn persist(&self, binding: &CredentialBinding, credential: &Credential) -> io::Result<()> {
        self.store(binding, credential)
    }
}

pub struct AuthoritySnapshot {
    pub credential: Arc<Credential>,
    pub generation: u64,
    pub relay_origin: String,
    pub profile: String,
    pub squad_id: String,
    pub squad_name: String,
    pub member_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleState {
    Starting,
    Ready,
    Leaving,
    Left,
    Recovering,
    RotationFailed,
    OutcomeUnknown,
}

struct SupervisorState {
    shared: Arc<RwLock<Shared>>,
    cancel: CancellationToken,
    task: StdMutex<Option<JoinHandle<()>>>,
    shutdown_bound: Duration,
    client: Arc<Client>,
    transport: Arc<dyn SessionTransport>,
    heartbeat_gate: Arc<tokio::sync::Mutex<()>>,
    binding: Arc<CredentialBinding>,
    store: Arc<CredentialStore>,
    metadata_path: std::path::PathBuf,
    squad_name: String,
    operation_gate: Arc<tokio::sync::Mutex<()>>,
    operation_cancel: CancellationToken,
    read_cancel: Arc<StdMutex<CancellationToken>>,
    shutdown_requested: AtomicBool,
    mode: AgentModeDto,
    client_metadata: ClientMetadata,
    profile_lock: StdMutex<Option<Arc<ProfileLock>>>,
    lifecycle: Arc<RwLock<LifecycleState>>,
    profile: ProfileBinding,
    journal: Arc<LeaveJournalStore>,
    journal_issuer: Arc<dyn JournalIssuer>,
    cleanup_seam: Arc<dyn CleanupSeam>,
    sends: Arc<SendLedger>,
    reports: Arc<ReportTracker>,
    #[cfg(test)]
    shutdown_gate_waiting: AtomicBool,
}

struct ReportTracker {
    active: AtomicUsize,
    drained: Notify,
    cancel: StdMutex<CancellationToken>,
    permanently_closed: AtomicBool,
}
const MAX_ACTIVE_REPORTS: usize = 8;

impl ReportTracker {
    fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            drained: Notify::new(),
            cancel: StdMutex::new(CancellationToken::new()),
            permanently_closed: AtomicBool::new(false),
        }
    }

    fn register(&self) -> Result<CancellationToken, SessionError> {
        let cancel = self
            .cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.permanently_closed.load(Ordering::Acquire) || cancel.is_cancelled() {
            return Err(SessionError::NotReady);
        }
        let mut active = self.active.load(Ordering::Acquire);
        loop {
            if active >= MAX_ACTIVE_REPORTS {
                return Err(SessionError::OperationCapacity);
            }
            match self.active.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => active = observed,
            }
        }
        Ok(cancel.clone())
    }

    fn finish(&self) {
        if self.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.drained.notify_waiters();
        }
    }

    fn cancel(&self) {
        self.cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel();
    }

    fn close_permanently(&self) {
        self.permanently_closed.store(true, Ordering::Release);
        self.cancel();
    }

    fn reopen(&self) {
        let mut cancel = self
            .cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.permanently_closed.load(Ordering::Acquire) {
            *cancel = CancellationToken::new();
        }
    }

    async fn drain(&self, bound: Duration) -> Result<(), SessionError> {
        tokio::time::timeout(bound, async {
            loop {
                let notified = self.drained.notified();
                if self.active.load(Ordering::Acquire) == 0 {
                    break;
                }
                notified.await;
            }
        })
        .await
        .map_err(|_| SessionError::ShutdownTimedOut)
    }
}

const MAX_OWNED_SENDS: usize = 64;
const MAX_OWNED_SEND_BYTES: usize = 1024 * 1024;
const OWNED_SEND_FIXED_BYTES: usize = 1024;

#[derive(Clone)]
enum StoredClientError {
    InvalidBaseUrl,
    InvalidConfiguration,
    InvalidRequest,
    MalformedCredential,
    OutcomeUnknown,
    Timeout,
    Api {
        status: u16,
        code: ApiErrorCode,
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
        last: Box<Self>,
    },
}

impl StoredClientError {
    fn capture(error: ClientError) -> Self {
        match error {
            ClientError::InvalidBaseUrl => Self::InvalidBaseUrl,
            ClientError::InvalidConfiguration => Self::InvalidConfiguration,
            ClientError::InvalidRequest => Self::InvalidRequest,
            ClientError::MalformedCredential => Self::MalformedCredential,
            ClientError::Transport(_) | ClientError::OutcomeUnknown => Self::OutcomeUnknown,
            ClientError::Timeout => Self::Timeout,
            ClientError::Api {
                status,
                code,
                retryable,
            } => Self::Api {
                status,
                code,
                retryable,
            },
            ClientError::MalformedResponse { status } => Self::MalformedResponse { status },
            ClientError::ResponseTooLarge => Self::ResponseTooLarge,
            ClientError::UnexpectedHttp { status } => Self::UnexpectedHttp { status },
            ClientError::ClientBusy => Self::ClientBusy,
            ClientError::RetryExhausted { attempts, last } => Self::RetryExhausted {
                attempts,
                last: Box::new(Self::capture(*last)),
            },
        }
    }

    fn restore(&self) -> ClientError {
        match self {
            Self::InvalidBaseUrl => ClientError::InvalidBaseUrl,
            Self::InvalidConfiguration => ClientError::InvalidConfiguration,
            Self::InvalidRequest => ClientError::InvalidRequest,
            Self::MalformedCredential => ClientError::MalformedCredential,
            Self::OutcomeUnknown => ClientError::OutcomeUnknown,
            Self::Timeout => ClientError::Timeout,
            Self::Api {
                status,
                code,
                retryable,
            } => ClientError::Api {
                status: *status,
                code: *code,
                retryable: *retryable,
            },
            Self::MalformedResponse { status } => {
                ClientError::MalformedResponse { status: *status }
            }
            Self::ResponseTooLarge => ClientError::ResponseTooLarge,
            Self::UnexpectedHttp { status } => ClientError::UnexpectedHttp { status: *status },
            Self::ClientBusy => ClientError::ClientBusy,
            Self::RetryExhausted { attempts, last } => ClientError::RetryExhausted {
                attempts: *attempts,
                last: Box::new(last.restore()),
            },
        }
    }
}

#[derive(Clone)]
enum StoredSendFailure {
    Relay(StoredClientError),
    NotReady,
}

type SendTerminal = Result<SendMessageResponse, StoredSendFailure>;

fn validate_owned_send_response(
    request: &PreparedSend,
    squad_name: &str,
    response: &SendMessageResponse,
) -> Result<(), StoredSendFailure> {
    let expected = request.request();
    let message = &response.message;
    if message.squad != squad_name
        || message.recipient != expected.recipient
        || message.body != expected.body
        || message.priority != expected.priority
        || message.reply_to != expected.reply_to
        || message.correlation_id != expected.correlation_id
    {
        return Err(StoredSendFailure::Relay(
            StoredClientError::MalformedResponse { status: 200 },
        ));
    }
    Ok(())
}

fn send_terminal_retained_bytes(result: &SendTerminal) -> usize {
    match result {
        Ok(response) => {
            let message = &response.message;
            OWNED_SEND_FIXED_BYTES
                .saturating_add(message.id.len())
                .saturating_add(message.squad.len())
                .saturating_add(message.sender.len())
                .saturating_add(message.recipient.len())
                .saturating_add(message.body.len())
                .saturating_add(message.reply_to.as_ref().map_or(0, String::len))
                .saturating_add(message.correlation_id.as_ref().map_or(0, String::len))
        }
        Err(_) => OWNED_SEND_FIXED_BYTES,
    }
}

struct SendEntry {
    retained_bytes: usize,
    terminal: StdMutex<Option<SendTerminal>>,
    notify: Notify,
}

struct SendLedgerInner {
    accepting: bool,
    retained_bytes: usize,
    inflight: usize,
    entries: HashMap<PreparedSendIdentity, Arc<SendEntry>>,
    terminal_fifo: VecDeque<PreparedSendIdentity>,
}

struct SendLedger {
    inner: tokio::sync::Mutex<SendLedgerInner>,
    drained: Notify,
    admission_open: AtomicBool,
    permanently_closed: AtomicBool,
}

enum SendReservation {
    Existing(Arc<SendEntry>),
    New(Arc<SendEntry>),
}

impl SendLedger {
    fn new() -> Self {
        Self {
            inner: tokio::sync::Mutex::new(SendLedgerInner {
                accepting: true,
                retained_bytes: 0,
                inflight: 0,
                entries: HashMap::new(),
                terminal_fifo: VecDeque::new(),
            }),
            drained: Notify::new(),
            admission_open: AtomicBool::new(true),
            permanently_closed: AtomicBool::new(false),
        }
    }

    async fn reserve(&self, request: &PreparedSend) -> Result<SendReservation, SessionError> {
        if self.permanently_closed.load(Ordering::Acquire)
            || !self.admission_open.load(Ordering::Acquire)
        {
            return Err(SessionError::NotReady);
        }
        let mut state = self.inner.lock().await;
        if self.permanently_closed.load(Ordering::Acquire)
            || !state.accepting
            || !self.admission_open.load(Ordering::Acquire)
        {
            return Err(SessionError::NotReady);
        }
        let key = request.operation_identity();
        if let Some(entry) = state.entries.get(&key) {
            return Ok(SendReservation::Existing(entry.clone()));
        }
        let request_value = request.request();
        let request_strings = request_value
            .recipient
            .len()
            .saturating_add(request_value.body.len())
            .saturating_add(request_value.dedupe_key.len())
            .saturating_add(request_value.reply_to.as_ref().map_or(0, String::len))
            .saturating_add(request_value.correlation_id.as_ref().map_or(0, String::len));
        // Account for the retained request plus a conservative terminal response copy
        // (which repeats routing/body/correlation strings) and bounded map metadata.
        let bytes = request_strings
            .saturating_mul(2)
            .saturating_add(OWNED_SEND_FIXED_BYTES);
        while (state.entries.len() >= MAX_OWNED_SENDS
            || state.retained_bytes.saturating_add(bytes) > MAX_OWNED_SEND_BYTES)
            && state.terminal_fifo.front().is_some()
        {
            let key = state
                .terminal_fifo
                .pop_front()
                .expect("guarded terminal key");
            if let Some(entry) = state.entries.remove(&key) {
                state.retained_bytes = state.retained_bytes.saturating_sub(entry.retained_bytes);
            }
        }
        if state.entries.len() >= MAX_OWNED_SENDS
            || state.retained_bytes.saturating_add(bytes) > MAX_OWNED_SEND_BYTES
        {
            return Err(SessionError::SendCapacity);
        }
        let entry = Arc::new(SendEntry {
            retained_bytes: bytes,
            terminal: StdMutex::new(None),
            notify: Notify::new(),
        });
        state.retained_bytes += bytes;
        state.inflight += 1;
        state.entries.insert(key, entry.clone());
        Ok(SendReservation::New(entry))
    }

    async fn finish(&self, key: PreparedSendIdentity, entry: &SendEntry, result: SendTerminal) {
        let retain_terminal = send_terminal_retained_bytes(&result) <= entry.retained_bytes;
        let mut state = self.inner.lock().await;
        *entry
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        state.inflight = state.inflight.saturating_sub(1);
        if retain_terminal {
            state.terminal_fifo.push_back(key);
        } else if state.entries.remove(&key).is_some() {
            state.retained_bytes = state.retained_bytes.saturating_sub(entry.retained_bytes);
        }
        entry.notify.notify_waiters();
        if state.inflight == 0 {
            self.drained.notify_waiters();
        }
    }

    async fn stop_and_drain(&self, bound: Duration) -> Result<(), SessionError> {
        self.stop_admission();
        {
            let mut state = self.inner.lock().await;
            state.accepting = false;
            if state.inflight == 0 {
                return Ok(());
            }
        }
        tokio::time::timeout(bound, async {
            loop {
                let notified = self.drained.notified();
                if self.inner.lock().await.inflight == 0 {
                    break;
                }
                notified.await;
            }
        })
        .await
        .map_err(|_| SessionError::ShutdownTimedOut)
    }

    fn stop_admission(&self) {
        self.admission_open.store(false, Ordering::Release);
    }

    async fn reopen_admission(&self) {
        if self.permanently_closed.load(Ordering::Acquire) {
            return;
        }
        let mut state = self.inner.lock().await;
        if self.permanently_closed.load(Ordering::Acquire) {
            return;
        }
        state.accepting = true;
        self.admission_open.store(true, Ordering::Release);
        if self.permanently_closed.load(Ordering::Acquire) {
            self.admission_open.store(false, Ordering::Release);
        }
    }

    fn close_permanently(&self) {
        self.permanently_closed.store(true, Ordering::Release);
        self.stop_admission();
    }
}

async fn await_send(entry: Arc<SendEntry>) -> Result<SendMessageResponse, SessionError> {
    loop {
        let notified = entry.notify.notified();
        if let Some(result) = entry
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .cloned()
        {
            return result.map_err(|error| match error {
                StoredSendFailure::Relay(error) => SessionError::Relay(error.restore()),
                StoredSendFailure::NotReady => SessionError::NotReady,
            });
        }
        notified.await;
    }
}

async fn read_with_epoch<T, F>(cancel: CancellationToken, operation: F) -> Result<T, SessionError>
where
    F: Future<Output = Result<T, ClientError>>,
{
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(SessionError::NotReady),
        result = operation => result.map_err(SessionError::Relay),
    }
}

impl SupervisorState {
    fn profile_lock(&self) -> Arc<ProfileLock> {
        self.profile_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .expect("active session retains profile ownership")
            .clone()
    }

    fn release_profile_lock(&self) {
        self.profile_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
        self.sends.close_permanently();
        self.reports.close_permanently();
        self.read_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel();
        self.operation_cancel.cancel();
        self.cancel.cancel();
    }

    fn take_scheduler(&self) -> Option<JoinHandle<()>> {
        self.task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    fn cleanup_confirmed_leave(&self) -> io::Result<()> {
        cleanup_confirmed_leave(
            &self.store,
            &self.metadata_path,
            &self.journal,
            self.cleanup_seam.as_ref(),
        )
    }

    async fn begin_leave(&self) -> Result<(), SessionError> {
        let read_cancel = self
            .read_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut lifecycle = self.lifecycle.write().await;
        match *lifecycle {
            LifecycleState::Ready => {
                read_cancel.cancel();
                *lifecycle = LifecycleState::Leaving;
                Ok(())
            }
            LifecycleState::Left => {
                drop(lifecycle);
                self.cleanup_confirmed_leave()
                    .map_err(SessionError::Local)?;
                Err(SessionError::Unbound)
            }
            _ => Err(SessionError::NotReady),
        }
    }

    async fn rollback_leave(&self) {
        {
            let mut read_cancel = self
                .read_cancel
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.shutdown_requested.load(Ordering::Acquire) {
                return;
            }
            *read_cancel = CancellationToken::new();
        }
        self.sends.reopen_admission().await;
        self.reports.reopen();
        if self.shutdown_requested.load(Ordering::Acquire) {
            return;
        }
        *self.lifecycle.write().await = LifecycleState::Ready;
    }
}

fn cleanup_confirmed_leave(
    store: &CredentialStore,
    metadata_path: &std::path::Path,
    journal: &LeaveJournalStore,
    seam: &dyn CleanupSeam,
) -> io::Result<()> {
    seam.before(CleanupStep::Credential)?;
    store.clear()?;
    seam.before(CleanupStep::Profile)?;
    clear_profile(metadata_path)?;
    seam.before(CleanupStep::Journal)?;
    journal.remove()
}

/// External handle to the profile-owned cooperative supervisor.
pub struct SessionRuntime {
    supervisor: Arc<SupervisorState>,
}

#[cfg(test)]
impl SessionRuntime {
    fn supervisor_mut(&mut self) -> &mut SupervisorState {
        Arc::get_mut(&mut self.supervisor).expect("test runtime has no cloned supervisor owner")
    }
}

#[allow(clippy::missing_errors_doc)]
impl SessionRuntime {
    fn state(&self) -> &SupervisorState {
        &self.supervisor
    }

    fn require_client_origin(client: &Client, relay_origin: &str) -> Result<(), SessionError> {
        let mut expected = url::Url::parse(relay_origin)
            .map_err(|_| SessionError::Relay(ClientError::InvalidBaseUrl))?;
        expected.set_path("/");
        if client.origin() != expected.as_str() {
            return Err(SessionError::Relay(ClientError::InvalidBaseUrl));
        }
        Ok(())
    }

    /// Replays a confirmed leave after metadata was already removed by an interrupted cleanup.
    ///
    /// # Errors
    /// Fails closed when an intent has no remaining profile metadata or local cleanup fails.
    pub async fn recover_orphaned_leave(
        paths: ProfilePaths,
        relay_origin: String,
        profile_name: String,
    ) -> Result<bool, SessionError> {
        tokio::spawn(async move {
            let _lock = ProfileLock::acquire(&paths.lock)?;
            if !paths.metadata.parent().is_some_and(std::path::Path::is_dir) {
                return Ok(false);
            }
            if crate::load_profile(&paths.metadata)?.is_some() {
                return Ok(false);
            }
            let journal = LeaveJournalStore::open(&paths.metadata)?;
            let Some((pending, _binding)) =
                journal.load_for_profile_key(&relay_origin, &profile_name)?
            else {
                return Ok(false);
            };
            if pending.phase() != LeavePhase::Confirmed {
                return Err(SessionError::RecoveryOutcomeUnknown);
            }
            let store = CredentialStore::open(paths.credential)?;
            cleanup_confirmed_leave(&store, &paths.metadata, &journal, &NoCleanupFault)?;
            Ok(true)
        })
        .await
        .map_err(|_| SessionError::Local(io::Error::other("leave recovery task failed")))?
    }

    /// Locks, loads, and validates one durable profile before returning readiness.
    ///
    /// # Errors
    /// Fails closed on local authority, lock, or initial relay validation failure.
    pub async fn start(client: Arc<Client>, spec: RuntimeSpec) -> Result<Self, SessionError> {
        Self::require_client_origin(&client, &spec.profile.relay_origin)?;
        tokio::spawn(async move {
            let lock = ProfileLock::acquire(&spec.paths.lock)?;
            if crate::load_profile(&spec.paths.metadata)?.as_ref() != Some(&spec.profile) {
                return Err(SessionError::Unbound);
            }
            Self::start_locked(client, spec, lock).await
        })
        .await
        .map_err(|_| SessionError::Local(io::Error::other("startup transaction task failed")))?
    }

    /// Joins once and publishes success only after credential, metadata, and heartbeat durability.
    ///
    /// # Errors
    /// Fails closed on a bound profile, preserves outcome-unknown, and scrubs only new authority.
    pub async fn join_and_bind(
        client: Arc<Client>,
        spec: UnboundRuntimeSpec,
        request: JoinSquadRequest,
    ) -> Result<JoinedRuntime, SessionError> {
        Self::require_client_origin(&client, &spec.relay_origin)?;
        let transport = client.clone() as Arc<dyn SessionTransport>;
        tokio::spawn(Self::join_and_bind_owned(client, transport, spec, request))
            .await
            .map_err(|_| SessionError::Local(io::Error::other("join transaction task failed")))?
    }

    async fn join_and_bind_owned(
        client: Arc<Client>,
        transport: Arc<dyn SessionTransport>,
        spec: UnboundRuntimeSpec,
        request: JoinSquadRequest,
    ) -> Result<JoinedRuntime, SessionError> {
        Self::require_client_origin(&client, &spec.relay_origin)?;
        let lock = ProfileLock::acquire(&spec.paths.lock)?;
        if spec.paths.metadata.exists() || spec.paths.credential.exists() {
            return Err(SessionError::Local(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "profile is already bound",
            )));
        }
        let session = transport
            .join(&spec.squad, &request)
            .await
            .map_err(SessionError::Relay)?;
        if session.response.squad.name != spec.squad
            || session.response.member_name != request.name
            || session.response.role != request.role
        {
            return Err(SessionError::Relay(ClientError::MalformedResponse {
                status: 200,
            }));
        }
        let profile = ProfileBinding::new(
            spec.profile_name,
            spec.relay_origin,
            session.response.squad.name.clone(),
            session.response.squad.id.clone(),
            session.response.membership_id.clone(),
        )
        .map_err(|_| {
            SessionError::Local(io::Error::new(
                io::ErrorKind::InvalidData,
                "joined profile is invalid",
            ))
        })?;
        let store = CredentialStore::open(spec.paths.credential.clone())?;
        let binding = CredentialBinding::new(
            &profile.relay_origin,
            &profile.profile,
            &profile.squad_id,
            &profile.member_id,
        )?;
        store.store(&binding, &session.credential)?;
        if let Err(error) = store_profile(&spec.paths.metadata, &profile) {
            store.clear()?;
            return Err(SessionError::Local(error));
        }
        let response = session.response.clone();
        let runtime_spec = RuntimeSpec {
            profile,
            paths: spec.paths,
            mode: request.mode,
            client_metadata: request.client,
            shutdown_bound: spec.shutdown_bound,
        };
        let runtime = Self::start_locked(client, runtime_spec, lock).await?;
        Ok(JoinedRuntime { runtime, response })
    }

    async fn start_locked(
        client: Arc<Client>,
        spec: RuntimeSpec,
        lock: ProfileLock,
    ) -> Result<Self, SessionError> {
        let transport = client.clone() as Arc<dyn SessionTransport>;
        Self::start_locked_with(
            client,
            transport,
            spec,
            lock,
            Arc::new(SystemJournalIssuer),
            Arc::new(NoCleanupFault),
        )
        .await
    }

    #[allow(clippy::too_many_lines)]
    async fn start_locked_with(
        client: Arc<Client>,
        transport: Arc<dyn SessionTransport>,
        spec: RuntimeSpec,
        lock: ProfileLock,
        journal_issuer: Arc<dyn JournalIssuer>,
        cleanup_seam: Arc<dyn CleanupSeam>,
    ) -> Result<Self, SessionError> {
        let ownership = Arc::new(lock);
        let store = Arc::new(CredentialStore::open(spec.paths.credential.clone())?);
        let journal = Arc::new(LeaveJournalStore::open(&spec.paths.metadata)?);
        let binding = Arc::new(CredentialBinding::new(
            &spec.profile.relay_origin,
            &spec.profile.profile,
            &spec.profile.squad_id,
            &spec.profile.member_id,
        )?);
        let pending_leave = journal.load(&spec.profile)?;
        if pending_leave
            .as_ref()
            .is_some_and(|value| value.phase() == LeavePhase::Confirmed)
        {
            cleanup_confirmed_leave(
                &store,
                &spec.paths.metadata,
                &journal,
                cleanup_seam.as_ref(),
            )?;
            return Err(SessionError::Unbound);
        }
        let mut credential = store.load(&binding)?;
        let lifecycle = Arc::new(RwLock::new(LifecycleState::Starting));
        let request = HeartbeatRequest {
            availability: AvailabilityDto::Unknown,
            availability_source: AvailabilitySourceDto::Unknown,
        };
        let mut generation = 0_u64;
        let heartbeat = match transport.heartbeat(&request, &credential).await {
            Ok(value) => {
                if pending_leave.is_some() {
                    journal.remove()?;
                }
                value
            }
            Err(ClientError::Api {
                code: ApiErrorCode::LeaseExpired,
                ..
            }) => {
                *lifecycle.write().await = LifecycleState::Recovering;
                let resumed = resume_transaction(
                    transport.clone(),
                    store.clone(),
                    binding.clone(),
                    ownership.clone(),
                    spec.profile.squad_name.clone(),
                    ResumeSquadRequest {
                        mode: spec.mode,
                        client: spec.client_metadata.clone(),
                    },
                    Arc::new(credential),
                )
                .await
                .map_err(resume_session_error)?;
                credential = resumed.credential;
                generation = 1;
                if pending_leave.is_some() {
                    journal.remove()?;
                }
                psst_protocol::HeartbeatResponse {
                    lease_expires_at: resumed.response.lease_expires_at,
                    heartbeat_interval_seconds: resumed.response.heartbeat_interval_seconds,
                }
            }
            Err(ClientError::Api {
                code: ApiErrorCode::NotMember | ApiErrorCode::SquadArchived,
                ..
            }) if pending_leave.is_some() => {
                let confirmed = journal_issuer
                    .confirm(pending_leave.as_ref().expect("guarded pending leave"))?;
                journal.store(&confirmed)?;
                cleanup_confirmed_leave(
                    &store,
                    &spec.paths.metadata,
                    &journal,
                    cleanup_seam.as_ref(),
                )?;
                return Err(SessionError::Unbound);
            }
            Err(error) => return Err(SessionError::Relay(error)),
        };
        let credential = Arc::new(credential);
        *lifecycle.write().await = LifecycleState::Ready;
        let instance_id = credential.instance_id().to_owned();
        let shared = Arc::new(RwLock::new(Shared {
            credential,
            instance_id,
            heartbeat_interval_seconds: heartbeat.heartbeat_interval_seconds,
            lease_expires_at: heartbeat.lease_expires_at,
            availability: AvailabilityDto::Unknown,
            availability_source: AvailabilitySourceDto::Unknown,
            health: SessionHealth::Ready,
            generation,
        }));
        let cancel = CancellationToken::new();
        let heartbeat_gate = Arc::new(tokio::sync::Mutex::new(()));
        let squad_name = spec.profile.squad_name.clone();
        let task = tokio::spawn(run_heartbeat(
            transport.clone(),
            store.clone(),
            binding.clone(),
            shared.clone(),
            lifecycle.clone(),
            cancel.clone(),
            spec.mode,
            spec.client_metadata.clone(),
            heartbeat_gate.clone(),
            squad_name.clone(),
            Arc::new(TokioClock),
            ownership.clone(),
        ));
        Ok(Self {
            supervisor: Arc::new(SupervisorState {
                shared,
                cancel,
                task: StdMutex::new(Some(task)),
                shutdown_bound: spec.shutdown_bound,
                profile_lock: StdMutex::new(Some(ownership)),
                client,
                transport,
                heartbeat_gate,
                binding,
                store,
                metadata_path: spec.paths.metadata,
                squad_name,
                operation_gate: Arc::new(tokio::sync::Mutex::new(())),
                operation_cancel: CancellationToken::new(),
                read_cancel: Arc::new(StdMutex::new(CancellationToken::new())),
                shutdown_requested: AtomicBool::new(false),
                mode: spec.mode,
                client_metadata: spec.client_metadata,
                lifecycle,
                profile: spec.profile,
                journal,
                journal_issuer,
                cleanup_seam,
                sends: Arc::new(SendLedger::new()),
                reports: Arc::new(ReportTracker::new()),
                #[cfg(test)]
                shutdown_gate_waiting: AtomicBool::new(false),
            }),
        })
    }

    pub async fn snapshot(&self) -> SessionSnapshot {
        let value = self.state().shared.read().await;
        SessionSnapshot {
            health: value.health,
            generation: value.generation,
            instance_id: value.instance_id.clone(),
            heartbeat_interval_seconds: value.heartbeat_interval_seconds,
            lease_expires_at: value.lease_expires_at,
            availability: value.availability,
            availability_source: value.availability_source,
        }
    }

    /// Returns one coherent authority generation only while the session is ready.
    ///
    /// # Errors
    /// Returns `NotReady` while identity outcome or credential rotation is uncertain.
    pub async fn authority(&self) -> Result<AuthoritySnapshot, SessionError> {
        if self.state().shutdown_requested.load(Ordering::Acquire) {
            return Err(SessionError::NotReady);
        }
        if *self.state().lifecycle.read().await != LifecycleState::Ready {
            return Err(SessionError::NotReady);
        }
        let (credential, generation, health) = {
            let value = self.state().shared.read().await;
            (value.credential.clone(), value.generation, value.health)
        };
        if health != SessionHealth::Ready
            || *self.state().lifecycle.read().await != LifecycleState::Ready
            || self.state().shutdown_requested.load(Ordering::Acquire)
        {
            return Err(SessionError::NotReady);
        }
        Ok(AuthoritySnapshot {
            credential,
            generation,
            relay_origin: self.state().binding.relay_origin().to_owned(),
            profile: self.state().binding.profile().to_owned(),
            squad_id: self.state().binding.squad_id().to_owned(),
            squad_name: self.state().squad_name.clone(),
            member_id: self.state().binding.member_id().to_owned(),
        })
    }

    /// Stops heartbeat first and clears local authority only after confirmed relay leave.
    ///
    /// # Errors
    /// Relay ambiguity retains credential and metadata; confirmed success may report cleanup failure.
    #[allow(clippy::too_many_lines)]
    pub async fn leave(&self) -> Result<LeaveSquadResponse, SessionError> {
        let supervisor = self.supervisor.clone();
        let response = tokio::spawn(async move {
            let _operation = supervisor.operation_gate.clone().lock_owned().await;
            supervisor.reports.cancel();
            if let Err(error) = supervisor.reports.drain(supervisor.shutdown_bound).await {
                if !supervisor.shutdown_requested.load(Ordering::Acquire)
                    && *supervisor.lifecycle.read().await == LifecycleState::Ready
                {
                    supervisor.reports.reopen();
                }
                return Err(error);
            }
            supervisor.begin_leave().await?;
            if let Err(error) = supervisor
                .sends
                .stop_and_drain(supervisor.shutdown_bound)
                .await
            {
                supervisor.rollback_leave().await;
                return Err(error);
            }
            let _heartbeat = supervisor.heartbeat_gate.clone().lock_owned().await;
            let credential = supervisor.shared.read().await.credential.clone();
            let intent = match supervisor.journal_issuer.intent(&supervisor.profile) {
                Ok(intent) => intent,
                Err(error) => {
                    supervisor.rollback_leave().await;
                    return Err(SessionError::Local(error));
                }
            };
            if let Err(error) = supervisor.journal.store(&intent) {
                supervisor.rollback_leave().await;
                return Err(SessionError::Local(error));
            }
            let response = match supervisor
                .transport
                .leave(&supervisor.squad_name, &credential)
                .await
            {
                Ok(response) if response.membership_id == supervisor.binding.member_id() => {
                    Ok(response)
                }
                Ok(_) => {
                    supervisor.shared.write().await.health = SessionHealth::OutcomeUnknown;
                    *supervisor.lifecycle.write().await = LifecycleState::OutcomeUnknown;
                    return Err(SessionError::Relay(ClientError::MalformedResponse {
                        status: 200,
                    }));
                }
                Err(
                    error @ ClientError::Api {
                        code: ApiErrorCode::NotMember | ApiErrorCode::SquadArchived,
                        ..
                    },
                ) => Err(error),
                Err(error @ ClientError::Api { .. }) => {
                    if let Err(local) = supervisor.journal.remove() {
                        supervisor.shared.write().await.health = SessionHealth::OutcomeUnknown;
                        *supervisor.lifecycle.write().await = LifecycleState::OutcomeUnknown;
                        return Err(SessionError::Local(local));
                    }
                    supervisor.rollback_leave().await;
                    return Err(SessionError::Relay(error));
                }
                Err(error) => {
                    supervisor.shared.write().await.health = SessionHealth::OutcomeUnknown;
                    *supervisor.lifecycle.write().await = LifecycleState::OutcomeUnknown;
                    return Err(SessionError::Relay(error));
                }
            };
            let confirmed = match supervisor.journal_issuer.confirm(&intent) {
                Ok(confirmed) => confirmed,
                Err(error) => {
                    supervisor.shared.write().await.health = SessionHealth::OutcomeUnknown;
                    *supervisor.lifecycle.write().await = LifecycleState::OutcomeUnknown;
                    return Err(SessionError::Local(error));
                }
            };
            if let Err(error) = supervisor.journal.store(&confirmed) {
                supervisor.shared.write().await.health = SessionHealth::OutcomeUnknown;
                *supervisor.lifecycle.write().await = LifecycleState::OutcomeUnknown;
                return Err(SessionError::Local(error));
            }
            // Once leave is confirmed, cleanup is replayable and has no cancellation point.
            supervisor.cancel.cancel();
            supervisor.operation_cancel.cancel();
            let cleanup = supervisor.cleanup_confirmed_leave();
            supervisor.shared.write().await.health = SessionHealth::Stopped;
            *supervisor.lifecycle.write().await = LifecycleState::Left;
            if let Some(mut task) = supervisor.take_scheduler()
                && tokio::time::timeout(supervisor.shutdown_bound, &mut task)
                    .await
                    .is_err()
            {
                task.abort();
                let _ = task.await;
                return Err(SessionError::ShutdownTimedOut);
            }
            cleanup.map_err(SessionError::Local)?;
            response.map_err(SessionError::Relay)
        })
        .await
        .map_err(|_| SessionError::Local(io::Error::other("leave transaction task failed")))??;
        Ok(response)
    }

    pub async fn archive(&self) -> Result<ArchiveSquadResponse, SessionError> {
        let _operation = tokio::select! {
            biased;
            () = self.state().operation_cancel.cancelled() => return Err(SessionError::NotReady),
            gate = self.state().operation_gate.lock() => gate,
        };
        let authority = self.authority().await?;
        tokio::select! {
            biased;
            () = self.state().operation_cancel.cancelled() => Err(SessionError::NotReady),
            result = self.state().client.archive_squad(&self.state().squad_name, &authority.credential) => result.map_err(SessionError::Relay),
        }
    }
    pub async fn roster(&self) -> Result<RosterResponse, SessionError> {
        let cancel = self
            .state()
            .read_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let authority = self.authority().await?;
        read_with_epoch(
            cancel,
            self.state()
                .client
                .roster(&self.state().squad_name, &authority.credential),
        )
        .await
    }
    pub async fn send_prepared(
        &self,
        request: &PreparedSend,
    ) -> Result<SendMessageResponse, SessionError> {
        let authority = self.authority().await?;
        let reservation = self.state().sends.reserve(request).await?;
        let entry = match reservation {
            SendReservation::Existing(entry) => entry,
            SendReservation::New(entry) => {
                let supervisor = self.supervisor.clone();
                let request = request.clone();
                let owned_entry = entry.clone();
                tokio::spawn(async move {
                    let key = request.operation_identity();
                    let _heartbeat = supervisor.heartbeat_gate.clone().lock_owned().await;
                    let lifecycle = *supervisor.lifecycle.read().await;
                    let state = supervisor.shared.read().await;
                    let authority_is_current = state.generation == authority.generation
                        && state.health == SessionHealth::Ready
                        && matches!(lifecycle, LifecycleState::Ready | LifecycleState::Leaving);
                    drop(state);
                    let result = if authority_is_current {
                        supervisor
                            .transport
                            .send_prepared(&request, &authority.credential)
                            .await
                            .map_err(|error| {
                                StoredSendFailure::Relay(StoredClientError::capture(error))
                            })
                            .and_then(|response| {
                                validate_owned_send_response(
                                    &request,
                                    &authority.squad_name,
                                    &response,
                                )?;
                                Ok(response)
                            })
                    } else {
                        Err(StoredSendFailure::NotReady)
                    };
                    supervisor.sends.finish(key, &owned_entry, result).await;
                });
                entry
            }
        };
        await_send(entry).await
    }
    pub async fn inbox(&self, limit: u16, wait_seconds: u8) -> Result<InboxResponse, SessionError> {
        let cancel = self
            .state()
            .read_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let authority = self.authority().await?;
        read_with_epoch(
            cancel,
            self.state()
                .client
                .inbox(limit, wait_seconds, &authority.credential),
        )
        .await
    }

    /// Signals all cooperative work to stop; `shutdown` performs bounded reaping.
    pub fn request_shutdown(&self) {
        self.state().request_shutdown();
    }
    pub async fn acknowledge(
        &self,
        request: &AckMessagesRequest,
    ) -> Result<AckMessagesResponse, SessionError> {
        let _operation = tokio::select! {
            biased;
            () = self.state().operation_cancel.cancelled() => return Err(SessionError::NotReady),
            gate = self.state().operation_gate.lock() => gate,
        };
        let authority = self.authority().await?;
        tokio::select! {
            biased;
            () = self.state().operation_cancel.cancelled() => Err(SessionError::NotReady),
            result = self.state().client.acknowledge(request, &authority.credential) => result.map_err(SessionError::Relay),
        }
    }
    pub async fn transcript(
        &self,
        after: MessageSequence,
        limit: u16,
    ) -> Result<TranscriptResponse, SessionError> {
        let cancel = self
            .state()
            .read_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let authority = self.authority().await?;
        read_with_epoch(
            cancel,
            self.state().client.transcript(
                &self.state().squad_name,
                after,
                limit,
                &authority.credential,
            ),
        )
        .await
    }

    /// Publishes explicit agent availability immediately without overlapping heartbeat traffic.
    ///
    /// # Errors
    /// Accepts `unknown` with an `unknown` source and returns sanitized relay failure.
    pub async fn report_availability(
        &self,
        availability: AvailabilityDto,
    ) -> Result<(), SessionError> {
        let source = if availability == AvailabilityDto::Unknown {
            AvailabilitySourceDto::Unknown
        } else {
            AvailabilitySourceDto::AgentReported
        };
        let supervisor = self.supervisor.clone();
        let cancel = supervisor.reports.register()?;
        let tracker = supervisor.reports.clone();
        tokio::spawn(async move {
            let result = report_availability_owned(supervisor, availability, source, cancel).await;
            tracker.finish();
            result
        })
        .await
        .map_err(|_| SessionError::Local(io::Error::other("availability owner task failed")))?
    }

    /// Stops heartbeat activity within the configured bound.
    ///
    /// # Errors
    /// Returns `ShutdownTimedOut` after aborting a task that fails to stop in time.
    pub async fn shutdown(&self) -> Result<(), SessionError> {
        self.state()
            .shutdown_requested
            .store(true, Ordering::Release);
        self.state().sends.close_permanently();
        self.state().reports.close_permanently();
        self.state()
            .reports
            .drain(self.state().shutdown_bound)
            .await?;
        self.state()
            .sends
            .stop_and_drain(self.state().shutdown_bound)
            .await?;
        self.state().operation_cancel.cancel();
        self.state()
            .read_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel();
        self.state().cancel.cancel();
        let task = self.state().take_scheduler();
        if let Some(mut task) = task
            && tokio::time::timeout(self.state().shutdown_bound, &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
            return Err(SessionError::ShutdownTimedOut);
        }
        // The operation gate is last in the shutdown drain order. Leave/archive/ack owners may
        // still be committing remote or local state after cancellation is requested, so profile
        // ownership cannot be released until that owner reaches its terminal publication.
        #[cfg(test)]
        self.state()
            .shutdown_gate_waiting
            .store(true, Ordering::Release);
        let _operation = tokio::time::timeout(
            self.state().shutdown_bound,
            self.state().operation_gate.lock(),
        )
        .await
        .map_err(|_| SessionError::ShutdownTimedOut)?;
        self.state().shared.write().await.health = SessionHealth::Stopped;
        self.state().release_profile_lock();
        Ok(())
    }
}

async fn report_availability_owned(
    supervisor: Arc<SupervisorState>,
    availability: AvailabilityDto,
    source: AvailabilitySourceDto,
    cancel: CancellationToken,
) -> Result<(), SessionError> {
    let _gate = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(SessionError::NotReady),
        gate = supervisor.heartbeat_gate.clone().lock_owned() => gate,
    };
    if supervisor.operation_cancel.is_cancelled()
        || *supervisor.lifecycle.read().await != LifecycleState::Ready
    {
        return Err(SessionError::NotReady);
    }
    let credential = supervisor.shared.read().await.credential.clone();
    let request = HeartbeatRequest {
        availability,
        availability_source: source,
    };
    let response = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(SessionError::NotReady),
        response = supervisor.transport.heartbeat(&request, &credential) => response,
    };
    if *supervisor.lifecycle.read().await != LifecycleState::Ready {
        return Err(SessionError::NotReady);
    }
    let response = match response {
        Ok(response) => response,
        Err(ClientError::Api {
            code: ApiErrorCode::LeaseExpired,
            ..
        }) => {
            let mut lifecycle = supervisor.lifecycle.write().await;
            if *lifecycle != LifecycleState::Ready {
                return Err(SessionError::NotReady);
            }
            *lifecycle = LifecycleState::Recovering;
            drop(lifecycle);
            let session = resume_transaction(
                supervisor.transport.clone(),
                supervisor.store.clone(),
                supervisor.binding.clone(),
                supervisor.profile_lock(),
                supervisor.squad_name.clone(),
                ResumeSquadRequest {
                    mode: supervisor.mode,
                    client: supervisor.client_metadata.clone(),
                },
                credential,
            )
            .await;
            let session = match session {
                Ok(session) => session,
                Err(error) => {
                    if *supervisor.lifecycle.read().await == LifecycleState::Leaving {
                        return Err(resume_session_error(error));
                    }
                    return Err(publish_resume_failure(&supervisor, error).await);
                }
            };
            let response = psst_protocol::HeartbeatResponse {
                lease_expires_at: session.response.lease_expires_at,
                heartbeat_interval_seconds: session.response.heartbeat_interval_seconds,
            };
            let mut state = supervisor.shared.write().await;
            state.instance_id = session.response.instance_id;
            state.credential = Arc::new(session.credential);
            state.generation = state.generation.saturating_add(1);
            state.availability = availability;
            state.availability_source = source;
            state.heartbeat_interval_seconds = response.heartbeat_interval_seconds;
            state.lease_expires_at = response.lease_expires_at;
            state.health = SessionHealth::Ready;
            drop(state);
            let mut lifecycle = supervisor.lifecycle.write().await;
            if *lifecycle != LifecycleState::Recovering {
                return Err(SessionError::NotReady);
            }
            *lifecycle = LifecycleState::Ready;
            return Ok(());
        }
        Err(error) => {
            if *supervisor.lifecycle.read().await == LifecycleState::Ready {
                supervisor.shared.write().await.health =
                    if matches!(error, ClientError::OutcomeUnknown) {
                        SessionHealth::OutcomeUnknown
                    } else {
                        SessionHealth::Degraded
                    };
            }
            return Err(SessionError::Relay(error));
        }
    };
    let mut state = supervisor.shared.write().await;
    state.availability = availability;
    state.availability_source = source;
    state.heartbeat_interval_seconds = response.heartbeat_interval_seconds;
    state.lease_expires_at = response.lease_expires_at;
    state.health = SessionHealth::Ready;
    drop(state);
    Ok(())
}

impl Drop for SessionRuntime {
    fn drop(&mut self) {
        self.state().request_shutdown();
        if let Some(task) = self.state().take_scheduler() {
            task.abort();
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_heartbeat(
    client: Arc<dyn SessionTransport>,
    store: Arc<dyn RotationStore>,
    binding: Arc<CredentialBinding>,
    shared: Arc<RwLock<Shared>>,
    lifecycle: Arc<RwLock<LifecycleState>>,
    cancel: CancellationToken,
    mode: AgentModeDto,
    metadata: ClientMetadata,
    heartbeat_gate: Arc<tokio::sync::Mutex<()>>,
    squad_name: String,
    clock: Arc<dyn SessionClock>,
    ownership: Arc<ProfileLock>,
) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let cadence = Duration::from_secs(shared.read().await.heartbeat_interval_seconds.into());
        tokio::select! { () = cancel.cancelled() => break, () = clock.sleep(cadence) => {} }
        let _gate = tokio::select! {
            () = cancel.cancelled() => break,
            gate = heartbeat_gate.lock() => gate,
        };
        if cancel.is_cancelled() {
            break;
        }
        match *lifecycle.read().await {
            LifecycleState::Leaving => continue,
            LifecycleState::Ready => {}
            _ => break,
        }
        let request = {
            let state = shared.read().await;
            HeartbeatRequest {
                availability: state.availability,
                availability_source: state.availability_source,
            }
        };
        let credential = shared.read().await.credential.clone();
        let heartbeat = tokio::select! {
            () = cancel.cancelled() => break,
            result = client.heartbeat(&request, &credential) => result,
        };
        if cancel.is_cancelled() {
            break;
        }
        match *lifecycle.read().await {
            LifecycleState::Leaving => continue,
            LifecycleState::Ready => {}
            _ => break,
        }
        match heartbeat {
            Ok(response) => {
                let mut state = shared.write().await;
                state.heartbeat_interval_seconds = response.heartbeat_interval_seconds;
                state.lease_expires_at = response.lease_expires_at;
                state.health = SessionHealth::Ready;
                backoff = Duration::from_secs(1);
            }
            Err(ClientError::Api {
                code: ApiErrorCode::LeaseExpired,
                ..
            }) => {
                let mut current = lifecycle.write().await;
                if *current != LifecycleState::Ready {
                    continue;
                }
                *current = LifecycleState::Recovering;
                drop(current);
                let resume = ResumeSquadRequest {
                    mode,
                    client: metadata.clone(),
                };
                let resumed = resume_transaction(
                    client.clone(),
                    store.clone(),
                    binding.clone(),
                    ownership.clone(),
                    squad_name.clone(),
                    resume,
                    credential,
                )
                .await;
                match resumed {
                    Ok(session) => {
                        let mut state = shared.write().await;
                        state.credential = Arc::new(session.credential);
                        state.instance_id = session.response.instance_id;
                        state.heartbeat_interval_seconds =
                            session.response.heartbeat_interval_seconds;
                        state.lease_expires_at = session.response.lease_expires_at;
                        state.generation = state.generation.saturating_add(1);
                        state.health = SessionHealth::Ready;
                        drop(state);
                        let mut lifecycle = lifecycle.write().await;
                        if *lifecycle != LifecycleState::Recovering {
                            break;
                        }
                        *lifecycle = LifecycleState::Ready;
                    }
                    Err(ResumeTransactionError::Relay(ClientError::OutcomeUnknown)) => {
                        let mut current = lifecycle.write().await;
                        if *current != LifecycleState::Recovering {
                            break;
                        }
                        shared.write().await.health = SessionHealth::OutcomeUnknown;
                        *current = LifecycleState::OutcomeUnknown;
                    }
                    Err(ResumeTransactionError::Persist(_)) => {
                        let mut current = lifecycle.write().await;
                        if *current != LifecycleState::Recovering {
                            break;
                        }
                        shared.write().await.health = SessionHealth::RotationFailed;
                        *current = LifecycleState::RotationFailed;
                    }
                    Err(ResumeTransactionError::Relay(_) | ResumeTransactionError::Task) => {
                        let mut current = lifecycle.write().await;
                        if *current != LifecycleState::Recovering {
                            break;
                        }
                        shared.write().await.health = SessionHealth::Degraded;
                        *current = LifecycleState::Ready;
                    }
                }
                if cancel.is_cancelled() {
                    break;
                }
            }
            Err(ClientError::OutcomeUnknown) => {
                shared.write().await.health = SessionHealth::OutcomeUnknown;
            }
            Err(_) => {
                shared.write().await.health = SessionHealth::Degraded;
                tokio::select! { () = cancel.cancelled() => break, () = clock.sleep(backoff) => {} }
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psst_client::ClientConfig;
    use std::process::Command as ChildProcess;
    use std::{
        collections::VecDeque,
        sync::{
            Mutex as StdMutex,
            atomic::{AtomicUsize, Ordering},
        },
    };
    use tokio::sync::{Mutex as TokioMutex, Notify, oneshot, watch};

    enum HeartbeatStep {
        Return(Result<psst_protocol::HeartbeatResponse, ClientError>),
        Block(Arc<Notify>),
        BlockReturn(
            Arc<Notify>,
            Result<psst_protocol::HeartbeatResponse, ClientError>,
        ),
    }
    enum ResumeStep {
        Return(Box<Result<psst_client::Session, ClientError>>),
        Block(Arc<Notify>),
        BlockReturn(Arc<Notify>, Box<Result<psst_client::Session, ClientError>>),
    }
    struct ScriptedTransport {
        heartbeats: TokioMutex<VecDeque<HeartbeatStep>>,
        resumes: TokioMutex<VecDeque<ResumeStep>>,
        heartbeat_calls: AtomicUsize,
        resume_calls: AtomicUsize,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
        join_block: Option<Arc<Notify>>,
    }
    impl SessionTransport for ScriptedTransport {
        fn join<'a>(
            &'a self,
            _squad: &'a str,
            _request: &'a JoinSquadRequest,
        ) -> TransportFuture<'a, psst_client::Session> {
            Box::pin(async move {
                if let Some(notify) = &self.join_block {
                    notify.notified().await;
                }
                Err(ClientError::Timeout)
            })
        }
        fn leave<'a>(
            &'a self,
            _squad: &'a str,
            _credential: &'a Credential,
        ) -> TransportFuture<'a, LeaveSquadResponse> {
            Box::pin(async { Err(ClientError::Timeout) })
        }
        fn heartbeat<'a>(
            &'a self,
            _request: &'a HeartbeatRequest,
            _credential: &'a Credential,
        ) -> TransportFuture<'a, psst_protocol::HeartbeatResponse> {
            Box::pin(async move {
                self.heartbeat_calls.fetch_add(1, Ordering::SeqCst);
                let active = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_in_flight.fetch_max(active, Ordering::SeqCst);
                let step = self
                    .heartbeats
                    .lock()
                    .await
                    .pop_front()
                    .unwrap_or(HeartbeatStep::Return(Err(ClientError::Timeout)));
                let result = match step {
                    HeartbeatStep::Return(value) => value,
                    HeartbeatStep::Block(notify) => {
                        notify.notified().await;
                        Err(ClientError::Timeout)
                    }
                    HeartbeatStep::BlockReturn(notify, result) => {
                        notify.notified().await;
                        result
                    }
                };
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
                result
            })
        }
        fn resume<'a>(
            &'a self,
            _squad: &'a str,
            _request: &'a ResumeSquadRequest,
            _credential: &'a Credential,
        ) -> TransportFuture<'a, psst_client::Session> {
            Box::pin(async move {
                self.resume_calls.fetch_add(1, Ordering::SeqCst);
                match self
                    .resumes
                    .lock()
                    .await
                    .pop_front()
                    .unwrap_or(ResumeStep::Return(Box::new(Err(ClientError::Timeout))))
                {
                    ResumeStep::Return(value) => *value,
                    ResumeStep::Block(notify) => {
                        notify.notified().await;
                        Err(ClientError::Timeout)
                    }
                    ResumeStep::BlockReturn(notify, result) => {
                        notify.notified().await;
                        *result
                    }
                }
            })
        }
    }
    struct LeaveBarrierTransport {
        inner: Arc<dyn SessionTransport>,
        dispatched: Arc<Notify>,
        release: Arc<Notify>,
    }
    struct IdentityMismatchTransport {
        inner: Arc<dyn SessionTransport>,
        mismatch_join: bool,
        mismatch_leave: bool,
    }
    impl SessionTransport for IdentityMismatchTransport {
        fn join<'a>(
            &'a self,
            squad: &'a str,
            request: &'a JoinSquadRequest,
        ) -> TransportFuture<'a, psst_client::Session> {
            Box::pin(async move {
                let mut session = self.inner.join(squad, request).await?;
                if self.mismatch_join {
                    session.response.role = "unrelated-role".into();
                }
                Ok(session)
            })
        }
        fn leave<'a>(
            &'a self,
            squad: &'a str,
            credential: &'a Credential,
        ) -> TransportFuture<'a, LeaveSquadResponse> {
            Box::pin(async move {
                let mut response = self.inner.leave(squad, credential).await?;
                if self.mismatch_leave {
                    response.membership_id = "mem_unrelated".into();
                }
                Ok(response)
            })
        }
        fn heartbeat<'a>(
            &'a self,
            request: &'a HeartbeatRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_protocol::HeartbeatResponse> {
            self.inner.heartbeat(request, credential)
        }
        fn resume<'a>(
            &'a self,
            squad: &'a str,
            request: &'a ResumeSquadRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_client::Session> {
            self.inner.resume(squad, request, credential)
        }
        fn send_prepared<'a>(
            &'a self,
            request: &'a PreparedSend,
            credential: &'a Credential,
        ) -> TransportFuture<'a, SendMessageResponse> {
            self.inner.send_prepared(request, credential)
        }
    }
    struct HeartbeatBarrierTransport {
        inner: Arc<dyn SessionTransport>,
        dispatched: Arc<Notify>,
        release: Arc<Notify>,
        first: std::sync::atomic::AtomicBool,
        heartbeat_calls: AtomicUsize,
    }
    impl SessionTransport for HeartbeatBarrierTransport {
        fn join<'a>(
            &'a self,
            squad: &'a str,
            request: &'a JoinSquadRequest,
        ) -> TransportFuture<'a, psst_client::Session> {
            self.inner.join(squad, request)
        }
        fn leave<'a>(
            &'a self,
            squad: &'a str,
            credential: &'a Credential,
        ) -> TransportFuture<'a, LeaveSquadResponse> {
            self.inner.leave(squad, credential)
        }
        fn heartbeat<'a>(
            &'a self,
            request: &'a HeartbeatRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_protocol::HeartbeatResponse> {
            Box::pin(async move {
                self.heartbeat_calls.fetch_add(1, Ordering::SeqCst);
                if self.first.swap(false, Ordering::SeqCst) {
                    self.dispatched.notify_one();
                    self.release.notified().await;
                }
                self.inner.heartbeat(request, credential).await
            })
        }
        fn resume<'a>(
            &'a self,
            squad: &'a str,
            request: &'a ResumeSquadRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_client::Session> {
            self.inner.resume(squad, request, credential)
        }
    }
    struct ResumeFailureBarrierTransport {
        inner: Arc<dyn SessionTransport>,
        resume_dispatched: Arc<Notify>,
        resume_release: Arc<Notify>,
        first_heartbeat: std::sync::atomic::AtomicBool,
        resume_calls: AtomicUsize,
    }
    impl SessionTransport for ResumeFailureBarrierTransport {
        fn join<'a>(
            &'a self,
            squad: &'a str,
            request: &'a JoinSquadRequest,
        ) -> TransportFuture<'a, psst_client::Session> {
            self.inner.join(squad, request)
        }
        fn leave<'a>(
            &'a self,
            squad: &'a str,
            credential: &'a Credential,
        ) -> TransportFuture<'a, LeaveSquadResponse> {
            self.inner.leave(squad, credential)
        }
        fn heartbeat<'a>(
            &'a self,
            request: &'a HeartbeatRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_protocol::HeartbeatResponse> {
            Box::pin(async move {
                if self.first_heartbeat.swap(false, Ordering::SeqCst) {
                    Err(ClientError::Api {
                        status: 409,
                        code: ApiErrorCode::LeaseExpired,
                        retryable: false,
                    })
                } else {
                    self.inner.heartbeat(request, credential).await
                }
            })
        }
        fn resume<'a>(
            &'a self,
            _squad: &'a str,
            _request: &'a ResumeSquadRequest,
            _credential: &'a Credential,
        ) -> TransportFuture<'a, psst_client::Session> {
            Box::pin(async move {
                self.resume_calls.fetch_add(1, Ordering::SeqCst);
                self.resume_dispatched.notify_one();
                self.resume_release.notified().await;
                Err(ClientError::OutcomeUnknown)
            })
        }
    }
    impl SessionTransport for LeaveBarrierTransport {
        fn join<'a>(
            &'a self,
            squad: &'a str,
            request: &'a JoinSquadRequest,
        ) -> TransportFuture<'a, psst_client::Session> {
            self.inner.join(squad, request)
        }
        fn leave<'a>(
            &'a self,
            squad: &'a str,
            credential: &'a Credential,
        ) -> TransportFuture<'a, LeaveSquadResponse> {
            Box::pin(async move {
                self.dispatched.notify_one();
                self.release.notified().await;
                self.inner.leave(squad, credential).await
            })
        }
        fn heartbeat<'a>(
            &'a self,
            request: &'a HeartbeatRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_protocol::HeartbeatResponse> {
            self.inner.heartbeat(request, credential)
        }
        fn resume<'a>(
            &'a self,
            squad: &'a str,
            request: &'a ResumeSquadRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_client::Session> {
            self.inner.resume(squad, request, credential)
        }
    }
    struct LeaveErrorTransport {
        inner: Arc<dyn SessionTransport>,
        error: StdMutex<Option<ClientError>>,
    }
    struct AmbiguousLeaveCounter {
        inner: Arc<dyn SessionTransport>,
        calls: AtomicUsize,
    }
    impl SessionTransport for AmbiguousLeaveCounter {
        fn join<'a>(
            &'a self,
            squad: &'a str,
            request: &'a JoinSquadRequest,
        ) -> TransportFuture<'a, psst_client::Session> {
            self.inner.join(squad, request)
        }
        fn leave<'a>(
            &'a self,
            _squad: &'a str,
            _credential: &'a Credential,
        ) -> TransportFuture<'a, LeaveSquadResponse> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Err(ClientError::OutcomeUnknown)
            })
        }
        fn heartbeat<'a>(
            &'a self,
            request: &'a HeartbeatRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_protocol::HeartbeatResponse> {
            self.inner.heartbeat(request, credential)
        }
        fn resume<'a>(
            &'a self,
            squad: &'a str,
            request: &'a ResumeSquadRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_client::Session> {
            self.inner.resume(squad, request, credential)
        }
    }
    struct FirstHeartbeatTransport {
        inner: Arc<dyn SessionTransport>,
        first: StdMutex<Option<ClientError>>,
        resume: TokioMutex<Option<psst_client::Session>>,
        heartbeat_calls: AtomicUsize,
        resume_calls: AtomicUsize,
    }
    impl SessionTransport for FirstHeartbeatTransport {
        fn join<'a>(
            &'a self,
            squad: &'a str,
            request: &'a JoinSquadRequest,
        ) -> TransportFuture<'a, psst_client::Session> {
            self.inner.join(squad, request)
        }
        fn leave<'a>(
            &'a self,
            squad: &'a str,
            credential: &'a Credential,
        ) -> TransportFuture<'a, LeaveSquadResponse> {
            self.inner.leave(squad, credential)
        }
        fn heartbeat<'a>(
            &'a self,
            request: &'a HeartbeatRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_protocol::HeartbeatResponse> {
            Box::pin(async move {
                self.heartbeat_calls.fetch_add(1, Ordering::SeqCst);
                let first = self.first.lock().unwrap().take();
                if let Some(error) = first {
                    Err(error)
                } else {
                    self.inner.heartbeat(request, credential).await
                }
            })
        }
        fn resume<'a>(
            &'a self,
            squad: &'a str,
            request: &'a ResumeSquadRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_client::Session> {
            Box::pin(async move {
                self.resume_calls.fetch_add(1, Ordering::SeqCst);
                if let Some(session) = self.resume.lock().await.take() {
                    Ok(session)
                } else {
                    self.inner.resume(squad, request, credential).await
                }
            })
        }
    }
    impl SessionTransport for LeaveErrorTransport {
        fn join<'a>(
            &'a self,
            squad: &'a str,
            request: &'a JoinSquadRequest,
        ) -> TransportFuture<'a, psst_client::Session> {
            self.inner.join(squad, request)
        }
        fn leave<'a>(
            &'a self,
            _squad: &'a str,
            _credential: &'a Credential,
        ) -> TransportFuture<'a, LeaveSquadResponse> {
            Box::pin(async move {
                Err(self
                    .error
                    .lock()
                    .unwrap()
                    .take()
                    .expect("scripted leave error consumed once"))
            })
        }
        fn heartbeat<'a>(
            &'a self,
            request: &'a HeartbeatRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_protocol::HeartbeatResponse> {
            self.inner.heartbeat(request, credential)
        }
        fn resume<'a>(
            &'a self,
            squad: &'a str,
            request: &'a ResumeSquadRequest,
            credential: &'a Credential,
        ) -> TransportFuture<'a, psst_client::Session> {
            self.inner.resume(squad, request, credential)
        }
    }
    struct ScriptedStore {
        fail_at: AtomicUsize,
        calls: AtomicUsize,
        persisted: StdMutex<Vec<String>>,
    }
    struct FixedJournalIssuer {
        created: ApiTimestamp,
        confirmed: ApiTimestamp,
        next: AtomicUsize,
    }
    struct FailingJournalIssuer;
    impl JournalIssuer for FailingJournalIssuer {
        fn intent(&self, _binding: &ProfileBinding) -> io::Result<LeaveJournal> {
            Err(io::Error::other("injected journal issuance failure"))
        }
        fn confirm(&self, _intent: &LeaveJournal) -> io::Result<LeaveJournal> {
            Err(io::Error::other("injected journal confirmation failure"))
        }
    }
    struct AbortOnConfirmIssuer {
        inner: Arc<dyn JournalIssuer>,
    }
    impl JournalIssuer for AbortOnConfirmIssuer {
        fn intent(&self, binding: &ProfileBinding) -> io::Result<LeaveJournal> {
            self.inner.intent(binding)
        }
        fn confirm(&self, _intent: &LeaveJournal) -> io::Result<LeaveJournal> {
            std::process::abort();
        }
    }
    impl JournalIssuer for FixedJournalIssuer {
        fn intent(&self, binding: &ProfileBinding) -> io::Result<LeaveJournal> {
            let sequence = self.next.fetch_add(1, Ordering::SeqCst);
            LeaveJournal::intent(binding, format!("fixed-operation-{sequence}"), self.created)
        }
        fn confirm(&self, intent: &LeaveJournal) -> io::Result<LeaveJournal> {
            intent.confirmed(self.confirmed)
        }
    }
    struct FailCleanupAt {
        step: CleanupStep,
        fired: std::sync::atomic::AtomicBool,
    }
    impl CleanupSeam for FailCleanupAt {
        fn before(&self, step: CleanupStep) -> io::Result<()> {
            if step == self.step && !self.fired.swap(true, Ordering::SeqCst) {
                Err(io::Error::other("injected cleanup failure"))
            } else {
                Ok(())
            }
        }
    }
    fn fixed_issuer() -> Arc<dyn JournalIssuer> {
        let created: ApiTimestamp = serde_json::from_str("\"2026-08-08T01:02:03.004Z\"").unwrap();
        let confirmed: ApiTimestamp = serde_json::from_str("\"2026-08-08T01:02:04.005Z\"").unwrap();
        Arc::new(FixedJournalIssuer {
            created,
            confirmed,
            next: AtomicUsize::new(0),
        })
    }
    #[derive(Default)]
    struct ManualClock {
        sleeps: StdMutex<VecDeque<(Duration, Arc<Notify>)>>,
    }
    impl SessionClock for ManualClock {
        fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let notify = Arc::new(Notify::new());
            self.sleeps
                .lock()
                .unwrap()
                .push_back((duration, notify.clone()));
            Box::pin(async move { notify.notified().await })
        }
    }
    impl ManualClock {
        async fn fire(&self, expected: Duration) {
            for _ in 0..100 {
                let next = { self.sleeps.lock().unwrap().pop_front() };
                if let Some((duration, notify)) = next {
                    assert_eq!(duration, expected);
                    notify.notify_one();
                    settle().await;
                    return;
                }
                tokio::task::yield_now().await;
            }
            panic!("scheduler did not register expected sleep");
        }
    }
    impl RotationStore for ScriptedStore {
        fn persist(&self, _binding: &CredentialBinding, credential: &Credential) -> io::Result<()> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call == self.fail_at.load(Ordering::SeqCst) {
                Err(io::Error::other("injected rotation persistence failure"))
            } else {
                self.persisted
                    .lock()
                    .unwrap()
                    .push(credential.instance_id().to_owned());
                Ok(())
            }
        }
    }
    async fn settle() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }
    async fn acquire_lock(path: &std::path::Path) -> ProfileLock {
        for _ in 0..100 {
            if let Ok(lock) = ProfileLock::acquire(path) {
                return lock;
            }
            tokio::task::yield_now().await;
        }
        ProfileLock::acquire(path).expect("profile ownership was not released")
    }

    fn crash_fixture_paths(root: &std::path::Path) -> ProfilePaths {
        ProfilePaths {
            metadata: root.join("profiles/default.json"),
            credential: root.join("credentials/default.json"),
            lock: root.join("runtime/default.lock"),
        }
    }

    fn write_crash_fixture_file(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        #[cfg(windows)]
        {
            use std::io::Write as _;
            let mut file = psst_platform_security::create_restricted_file(
                path,
                &psst_platform_security::current_process_sid().unwrap(),
            )
            .unwrap();
            file.write_all(b"crash-fixture").unwrap();
            file.sync_all().unwrap();
        }
        #[cfg(unix)]
        {
            use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .unwrap();
            file.write_all(b"crash-fixture").unwrap();
            file.sync_all().unwrap();
        }
    }

    #[test]
    #[ignore = "subprocess crash fixture"]
    fn abrupt_c2_child() {
        let root = std::path::PathBuf::from(std::env::var_os("PSST_C2_CRASH_ROOT").unwrap());
        let state = std::env::var("PSST_C2_CRASH_STATE").unwrap();
        let paths = crash_fixture_paths(&root);
        std::fs::create_dir_all(paths.metadata.parent().unwrap()).unwrap();
        let profile = ProfileBinding::new(
            "default".into(),
            "http://127.0.0.1:9".into(),
            "alpha".into(),
            "sqd_alpha".into(),
            "mem_worker".into(),
        )
        .unwrap();
        store_profile(&paths.metadata, &profile).unwrap();
        write_crash_fixture_file(&paths.credential);
        let journal = LeaveJournalStore::open(&paths.metadata).unwrap();
        let intent = fixed_issuer().intent(&profile).unwrap();
        journal.store(&intent).unwrap();
        if state != "intent" {
            journal
                .store(&fixed_issuer().confirm(&intent).unwrap())
                .unwrap();
        }
        if matches!(state.as_str(), "after_credential" | "after_profile") {
            CredentialStore::open(paths.credential.clone())
                .unwrap()
                .clear()
                .unwrap();
        }
        if matches!(state.as_str(), "intent" | "after_profile") {
            clear_profile(&paths.metadata).unwrap();
        }
        std::process::abort();
    }

    #[tokio::test]
    #[ignore = "subprocess crash fixture"]
    async fn abrupt_after_real_terminal_child() {
        let root = std::path::PathBuf::from(std::env::var_os("PSST_C2_CRASH_ROOT").unwrap());
        let address: std::net::SocketAddr = std::env::var("PSST_C2_RELAY_ADDRESS")
            .unwrap()
            .parse()
            .unwrap();
        let mut relay = psst_relay::RelayConfig::local(root.join("relay.db"));
        relay.bind = address;
        let (_shutdown, shutdown_rx) = watch::channel(false);
        tokio::spawn(psst_relay::serve(relay, shutdown_rx));
        let origin = format!("http://{address}");
        let client = Arc::new(Client::new(&origin, ClientConfig::default()).unwrap());
        for _ in 0..100 {
            if client.health().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let paths = crash_fixture_paths(&root);
        let mut runtime = SessionRuntime::join_and_bind(
            client,
            UnboundRuntimeSpec {
                relay_origin: origin,
                profile_name: "default".into(),
                squad: "terminal-squad".into(),
                paths,
                shutdown_bound: Duration::from_secs(1),
            },
            JoinSquadRequest {
                name: "terminal-worker".into(),
                role: "test".into(),
                mode: AgentModeDto::Cooperative,
                client: ClientMetadata {
                    kind: "test".into(),
                    hostname: None,
                    version: None,
                },
                mission: Some("real terminal crash fixture".into()),
            },
        )
        .await
        .unwrap()
        .runtime;
        runtime.supervisor_mut().journal_issuer = Arc::new(AbortOnConfirmIssuer {
            inner: fixed_issuer(),
        });
        let _ = runtime.leave().await;
        panic!("confirmation hook did not abort");
    }

    #[tokio::test]
    async fn abrupt_c2_boundaries_restart_fail_closed_or_finish_confirmed_cleanup() {
        for state in ["intent", "confirmed", "after_credential", "after_profile"] {
            let temp = tempfile::tempdir().unwrap();
            let status = ChildProcess::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "session::tests::abrupt_c2_child",
                    "--nocapture",
                ])
                .env("PSST_C2_CRASH_ROOT", temp.path())
                .env("PSST_C2_CRASH_STATE", state)
                .status()
                .unwrap();
            assert!(!status.success());
            let paths = crash_fixture_paths(temp.path());
            let recovery = SessionRuntime::recover_orphaned_leave(
                paths.clone(),
                "http://127.0.0.1:9".into(),
                "default".into(),
            )
            .await;
            if state == "intent" {
                assert!(matches!(
                    recovery,
                    Err(SessionError::RecoveryOutcomeUnknown)
                ));
                assert!(
                    crate::leave_journal::sibling_path(&paths.metadata)
                        .unwrap()
                        .exists()
                );
            } else {
                if paths.metadata.exists() {
                    let profile = crate::load_profile(&paths.metadata).unwrap().unwrap();
                    let client = Arc::new(
                        Client::new("http://127.0.0.1:9", ClientConfig::default()).unwrap(),
                    );
                    assert!(matches!(
                        SessionRuntime::start(
                            client,
                            RuntimeSpec {
                                profile,
                                paths: paths.clone(),
                                mode: AgentModeDto::Cooperative,
                                client_metadata: ClientMetadata {
                                    kind: "test".into(),
                                    hostname: None,
                                    version: None,
                                },
                                shutdown_bound: Duration::from_secs(1),
                            },
                        )
                        .await,
                        Err(SessionError::Unbound)
                    ));
                } else {
                    assert!(recovery.unwrap());
                }
                assert!(!paths.metadata.exists());
                assert!(!paths.credential.exists());
                assert!(
                    !crate::leave_journal::sibling_path(&paths.metadata)
                        .unwrap()
                        .exists()
                );
            }
        }
    }

    #[tokio::test]
    async fn restart_after_real_terminal_commit_before_confirmation_finishes_leave() {
        let temp = tempfile::tempdir().unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let status = ChildProcess::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "session::tests::abrupt_after_real_terminal_child",
                "--nocapture",
            ])
            .env("PSST_C2_CRASH_ROOT", temp.path())
            .env("PSST_C2_RELAY_ADDRESS", address.to_string())
            .status()
            .unwrap();
        assert!(!status.success());
        let paths = crash_fixture_paths(temp.path());
        let profile = crate::load_profile(&paths.metadata).unwrap().unwrap();
        let journal = LeaveJournalStore::open(&paths.metadata).unwrap();
        assert_eq!(
            journal.load(&profile).unwrap().unwrap().phase(),
            LeavePhase::Intent
        );

        let mut relay = psst_relay::RelayConfig::local(temp.path().join("relay.db"));
        relay.bind = address;
        let (shutdown, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(psst_relay::serve(relay, shutdown_rx));
        let client =
            Arc::new(Client::new(&format!("http://{address}"), ClientConfig::default()).unwrap());
        for _ in 0..100 {
            if client.health().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(matches!(
            SessionRuntime::start(
                client,
                RuntimeSpec {
                    profile,
                    paths: paths.clone(),
                    mode: AgentModeDto::Cooperative,
                    client_metadata: ClientMetadata {
                        kind: "test".into(),
                        hostname: None,
                        version: None,
                    },
                    shutdown_bound: Duration::from_secs(1),
                },
            )
            .await,
            Err(SessionError::Unbound)
        ));
        assert!(!paths.metadata.exists() && !paths.credential.exists());
        assert!(
            !crate::leave_journal::sibling_path(&paths.metadata)
                .unwrap()
                .exists()
        );
        shutdown.send(true).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn orphan_recovery_waiting_for_lock_never_deletes_new_authority() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crash_fixture_paths(temp.path());
        std::fs::create_dir_all(paths.metadata.parent().unwrap()).unwrap();
        let old = ProfileBinding::new(
            "default".into(),
            "http://127.0.0.1:9".into(),
            "old-squad".into(),
            "sqd_old".into(),
            "mem_old".into(),
        )
        .unwrap();
        let journal = LeaveJournalStore::open(&paths.metadata).unwrap();
        let intent = fixed_issuer().intent(&old).unwrap();
        journal
            .store(&fixed_issuer().confirm(&intent).unwrap())
            .unwrap();
        let ownership = ProfileLock::acquire(&paths.lock).unwrap();
        let recovery = tokio::spawn(SessionRuntime::recover_orphaned_leave(
            paths.clone(),
            old.relay_origin.clone(),
            old.profile.clone(),
        ));
        settle().await;
        let fresh = ProfileBinding::new(
            "default".into(),
            "http://127.0.0.1:9".into(),
            "fresh-squad".into(),
            "sqd_fresh".into(),
            "mem_fresh".into(),
        )
        .unwrap();
        store_profile(&paths.metadata, &fresh).unwrap();
        write_crash_fixture_file(&paths.credential);
        drop(ownership);
        assert!(matches!(
            recovery.await.unwrap(),
            Err(SessionError::Local(_))
        ));
        assert!(
            !SessionRuntime::recover_orphaned_leave(paths.clone(), old.relay_origin, old.profile,)
                .await
                .unwrap()
        );
        assert_eq!(crate::load_profile(&paths.metadata).unwrap(), Some(fresh));
        assert!(paths.credential.exists());
        assert!(
            crate::leave_journal::sibling_path(&paths.metadata)
                .unwrap()
                .exists()
        );
    }

    #[tokio::test]
    async fn dropped_join_waiter_retains_profile_ownership_until_issuance_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ProfilePaths {
            metadata: temp.path().join("profile.json"),
            credential: temp.path().join("credential.json"),
            lock: temp.path().join("runtime/profile.lock"),
        };
        let blocker = Arc::new(Notify::new());
        let transport = Arc::new(ScriptedTransport {
            heartbeats: TokioMutex::new(VecDeque::new()),
            resumes: TokioMutex::new(VecDeque::new()),
            heartbeat_calls: AtomicUsize::new(0),
            resume_calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            join_block: Some(blocker.clone()),
        });
        let client = Arc::new(Client::new("http://127.0.0.1:9", ClientConfig::default()).unwrap());
        let task = tokio::spawn(SessionRuntime::join_and_bind_owned(
            client,
            transport,
            UnboundRuntimeSpec {
                relay_origin: "http://127.0.0.1:9".into(),
                profile_name: "drop-test".into(),
                squad: "alpha".into(),
                paths: paths.clone(),
                shutdown_bound: Duration::from_secs(1),
            },
            JoinSquadRequest {
                name: "worker".into(),
                role: "test".into(),
                mode: AgentModeDto::Cooperative,
                client: ClientMetadata {
                    kind: "test".into(),
                    hostname: None,
                    version: None,
                },
                mission: None,
            },
        ));
        settle().await;
        assert!(ProfileLock::acquire(&paths.lock).is_err());
        drop(task);
        assert!(ProfileLock::acquire(&paths.lock).is_err());
        blocker.notify_one();
        for _ in 0..100 {
            if ProfileLock::acquire(&paths.lock).is_ok() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("owned join did not release profile after bounded issuance completed");
    }
    fn files_containing(
        root: &std::path::Path,
        needle: &[u8],
        found: &mut Vec<std::path::PathBuf>,
    ) {
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files_containing(&path, needle, found);
            } else if std::fs::read(&path)
                .is_ok_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
            {
                found.push(path);
            }
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn real_relay_join_is_durable_locked_validated_and_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let mut relay = psst_relay::RelayConfig::local(temp.path().join("relay.db"));
        relay.bind = address;
        let (shutdown, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(psst_relay::serve(relay, shutdown_rx));
        let origin = format!("http://{address}");
        let client = Arc::new(Client::new(&origin, ClientConfig::default()).unwrap());
        for _ in 0..100 {
            if client.health().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let origin_paths = ProfilePaths {
            metadata: temp.path().join("origin-fence/profile.json"),
            credential: temp.path().join("origin-fence/credential.json"),
            lock: temp.path().join("origin-fence/runtime.lock"),
        };
        let wrong_origin =
            Arc::new(Client::new("http://127.0.0.1:9", ClientConfig::default()).unwrap());
        let start_profile = ProfileBinding::new(
            "default".into(),
            origin.clone(),
            "alpha".into(),
            "sqd_alpha".into(),
            "mem_worker".into(),
        )
        .unwrap();
        assert!(matches!(
            SessionRuntime::start(
                wrong_origin.clone(),
                RuntimeSpec {
                    profile: start_profile,
                    paths: origin_paths.clone(),
                    mode: AgentModeDto::Cooperative,
                    client_metadata: ClientMetadata {
                        kind: "test".into(),
                        hostname: None,
                        version: None,
                    },
                    shutdown_bound: Duration::from_secs(1),
                },
            )
            .await,
            Err(SessionError::Relay(ClientError::InvalidBaseUrl))
        ));
        assert!(matches!(
            SessionRuntime::join_and_bind(
                wrong_origin,
                UnboundRuntimeSpec {
                    relay_origin: origin.clone(),
                    profile_name: "origin-join".into(),
                    squad: "origin-join".into(),
                    paths: origin_paths.clone(),
                    shutdown_bound: Duration::from_secs(1),
                },
                JoinSquadRequest {
                    name: "origin-worker".into(),
                    role: "test".into(),
                    mode: AgentModeDto::Cooperative,
                    client: ClientMetadata {
                        kind: "test".into(),
                        hostname: None,
                        version: None,
                    },
                    mission: None,
                },
            )
            .await,
            Err(SessionError::Relay(ClientError::InvalidBaseUrl))
        ));
        assert!(!origin_paths.metadata.exists() && !origin_paths.credential.exists());

        let mismatch_paths = ProfilePaths {
            metadata: temp.path().join("join-identity/profile.json"),
            credential: temp.path().join("join-identity/credential.json"),
            lock: temp.path().join("join-identity/runtime.lock"),
        };
        assert!(matches!(
            SessionRuntime::join_and_bind_owned(
                client.clone(),
                Arc::new(IdentityMismatchTransport {
                    inner: client.clone(),
                    mismatch_join: true,
                    mismatch_leave: false,
                }),
                UnboundRuntimeSpec {
                    relay_origin: origin.clone(),
                    profile_name: "join-identity".into(),
                    squad: "join-identity".into(),
                    paths: mismatch_paths.clone(),
                    shutdown_bound: Duration::from_secs(1),
                },
                JoinSquadRequest {
                    name: "identity-worker".into(),
                    role: "test".into(),
                    mode: AgentModeDto::Cooperative,
                    client: ClientMetadata {
                        kind: "test".into(),
                        hostname: None,
                        version: None,
                    },
                    mission: Some("identity validation".into()),
                },
            )
            .await,
            Err(SessionError::Relay(ClientError::MalformedResponse {
                status: 200
            }))
        ));
        assert!(!mismatch_paths.metadata.exists() && !mismatch_paths.credential.exists());
        let blocked_parent = temp.path().join("not-a-directory");
        std::fs::write(&blocked_parent, b"x").unwrap();
        let failed_credential = temp.path().join("fault-credential.json");
        let failed = SessionRuntime::join_and_bind(
            client.clone(),
            UnboundRuntimeSpec {
                relay_origin: origin.clone(),
                profile_name: "fault".into(),
                squad: "fault-squad".into(),
                paths: ProfilePaths {
                    metadata: blocked_parent.join("profile.json"),
                    credential: failed_credential.clone(),
                    lock: temp.path().join("fault.lock"),
                },
                shutdown_bound: Duration::from_secs(1),
            },
            JoinSquadRequest {
                name: "fault-worker".into(),
                role: "test".into(),
                mode: AgentModeDto::Cooperative,
                client: ClientMetadata {
                    kind: "test".into(),
                    hostname: None,
                    version: None,
                },
                mission: Some("fault test".into()),
            },
        )
        .await;
        assert!(matches!(failed, Err(SessionError::Local(_))));
        assert!(!failed_credential.exists());
        let paths = ProfilePaths {
            metadata: temp.path().join("profile.json"),
            credential: temp.path().join("credential.json"),
            lock: temp.path().join("runtime").join("profile.lock"),
        };
        let mut joined = SessionRuntime::join_and_bind(
            client.clone(),
            UnboundRuntimeSpec {
                relay_origin: origin.clone(),
                profile_name: "default".into(),
                squad: "alpha".into(),
                paths: paths.clone(),
                shutdown_bound: Duration::from_secs(1),
            },
            JoinSquadRequest {
                name: "worker".into(),
                role: "test".into(),
                mode: AgentModeDto::Cooperative,
                client: ClientMetadata {
                    kind: "test".into(),
                    hostname: None,
                    version: None,
                },
                mission: Some("runtime test".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(joined.response.member_name, "worker");
        assert_eq!(joined.runtime.snapshot().await.health, SessionHealth::Ready);
        assert!(paths.metadata.is_file());
        assert!(paths.credential.is_file());
        let credential_record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&paths.credential).unwrap()).unwrap();
        let credential_canary = credential_record["authorization"]
            .as_str()
            .unwrap()
            .as_bytes();
        let mut authority_files = Vec::new();
        files_containing(temp.path(), credential_canary, &mut authority_files);
        assert_eq!(
            authority_files.as_slice(),
            std::slice::from_ref(&paths.credential)
        );
        let binding = crate::load_profile(&paths.metadata).unwrap().unwrap();
        assert!(
            SessionRuntime::start(
                client.clone(),
                RuntimeSpec {
                    profile: binding,
                    paths: paths.clone(),
                    mode: AgentModeDto::Cooperative,
                    client_metadata: ClientMetadata {
                        kind: "test".into(),
                        hostname: None,
                        version: None
                    },
                    shutdown_bound: Duration::from_secs(1)
                }
            )
            .await
            .is_err()
        );
        joined
            .runtime
            .report_availability(AvailabilityDto::Busy)
            .await
            .unwrap();
        let production_transport = joined.runtime.supervisor.transport.clone();
        joined.runtime.supervisor_mut().transport = Arc::new(LeaveErrorTransport {
            inner: production_transport.clone(),
            error: StdMutex::new(Some(ClientError::Api {
                status: 400,
                code: ApiErrorCode::InvalidRequest,
                retryable: false,
            })),
        });
        assert!(matches!(
            joined.runtime.leave().await,
            Err(SessionError::Relay(ClientError::Api {
                code: ApiErrorCode::InvalidRequest,
                ..
            }))
        ));
        assert!(paths.metadata.exists() && paths.credential.exists());
        assert!(joined.runtime.supervisor.task.lock().unwrap().is_some());
        joined.runtime.supervisor_mut().transport = production_transport;
        let ((), inbox_result) = tokio::join!(
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                joined
                    .runtime
                    .supervisor
                    .read_cancel
                    .lock()
                    .unwrap()
                    .cancel();
            },
            joined.runtime.inbox(1, 5),
        );
        assert!(matches!(inbox_result, Err(SessionError::NotReady)));
        *joined.runtime.supervisor.read_cancel.lock().unwrap() = CancellationToken::new();

        // Replace the live scheduler with a deterministic scripted transport. The credentials
        // are issued by the real relay so rotation still exercises the real authority type.
        let old = joined.runtime.authority().await.unwrap();
        let resume_request = ResumeSquadRequest {
            mode: AgentModeDto::Cooperative,
            client: ClientMetadata {
                kind: "test".into(),
                hostname: None,
                version: None,
            },
        };
        let mut rotated = client
            .join(
                "rotation-source",
                &JoinSquadRequest {
                    name: "rotated-worker".into(),
                    role: "test".into(),
                    mode: AgentModeDto::Cooperative,
                    client: resume_request.client.clone(),
                    mission: Some("credential rotation fixture".into()),
                },
            )
            .await
            .unwrap();
        rotated.response.squad = joined.response.squad.clone();
        rotated.response.membership_id = joined.response.membership_id.clone();
        let rotated_cadence =
            Duration::from_secs(rotated.response.heartbeat_interval_seconds.into());
        let mut failed_rotation = client
            .join(
                "rotation-failure-source",
                &JoinSquadRequest {
                    name: "failed-rotation-worker".into(),
                    role: "test".into(),
                    mode: AgentModeDto::Cooperative,
                    client: resume_request.client.clone(),
                    mission: Some("failed credential rotation fixture".into()),
                },
            )
            .await
            .unwrap();
        failed_rotation.response.squad = joined.response.squad.clone();
        failed_rotation.response.membership_id = joined.response.membership_id.clone();
        let mut immediate_rotation = client
            .join(
                "immediate-rotation-source",
                &JoinSquadRequest {
                    name: "immediate-rotated-worker".into(),
                    role: "test".into(),
                    mode: AgentModeDto::Cooperative,
                    client: resume_request.client.clone(),
                    mission: Some("dropped waiter rotation fixture".into()),
                },
            )
            .await
            .unwrap();
        immediate_rotation.response.squad = joined.response.squad.clone();
        immediate_rotation.response.membership_id = joined.response.membership_id.clone();
        let mismatched_rotation = client
            .join(
                "binding-mismatch-source",
                &JoinSquadRequest {
                    name: "binding-mismatch-worker".into(),
                    role: "test".into(),
                    mode: AgentModeDto::Cooperative,
                    client: resume_request.client.clone(),
                    mission: Some("binding mismatch fixture".into()),
                },
            )
            .await
            .unwrap();
        let mismatch_store = Arc::new(ScriptedStore {
            fail_at: AtomicUsize::new(usize::MAX),
            calls: AtomicUsize::new(0),
            persisted: StdMutex::new(Vec::new()),
        });
        let mismatch_transport = Arc::new(ScriptedTransport {
            heartbeats: TokioMutex::new(VecDeque::new()),
            resumes: TokioMutex::new(VecDeque::from([ResumeStep::Return(Box::new(Ok(
                mismatched_rotation,
            )))])),
            heartbeat_calls: AtomicUsize::new(0),
            resume_calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            join_block: None,
        });
        let generation_before_mismatch = joined.runtime.snapshot().await.generation;
        assert!(matches!(
            resume_transaction(
                mismatch_transport,
                mismatch_store.clone(),
                joined.runtime.supervisor.binding.clone(),
                joined.runtime.supervisor.profile_lock(),
                "alpha".into(),
                resume_request.clone(),
                old.credential.clone(),
            )
            .await,
            Err(ResumeTransactionError::Relay(
                ClientError::MalformedResponse { status: 200 }
            ))
        ));
        assert_eq!(mismatch_store.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            joined.runtime.snapshot().await.generation,
            generation_before_mismatch
        );
        joined.runtime.supervisor.cancel.cancel();
        let scheduler = joined
            .runtime
            .supervisor
            .task
            .lock()
            .unwrap()
            .take()
            .unwrap();
        scheduler.await.unwrap();
        joined
            .runtime
            .supervisor
            .shared
            .write()
            .await
            .heartbeat_interval_seconds = 2;
        let blocker = Arc::new(Notify::new());
        let clock = Arc::new(ManualClock::default());
        let scripted = Arc::new(ScriptedTransport {
            heartbeats: TokioMutex::new(VecDeque::from([
                HeartbeatStep::Return(Ok(psst_protocol::HeartbeatResponse {
                    lease_expires_at: joined.runtime.snapshot().await.lease_expires_at,
                    heartbeat_interval_seconds: 5,
                })),
                HeartbeatStep::Return(Err(ClientError::Api {
                    status: 409,
                    code: ApiErrorCode::LeaseExpired,
                    retryable: false,
                })),
                HeartbeatStep::Return(Err(ClientError::OutcomeUnknown)),
                HeartbeatStep::Return(Err(ClientError::Timeout)),
                HeartbeatStep::Block(blocker.clone()),
            ])),
            resumes: TokioMutex::new(VecDeque::from([ResumeStep::Return(Box::new(Ok(rotated)))])),
            heartbeat_calls: AtomicUsize::new(0),
            resume_calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            join_block: None,
        });
        let persistence = Arc::new(ScriptedStore {
            fail_at: AtomicUsize::new(usize::MAX),
            calls: AtomicUsize::new(0),
            persisted: StdMutex::new(Vec::new()),
        });
        let cancel = CancellationToken::new();
        let scheduler = tokio::spawn(run_heartbeat(
            scripted.clone(),
            persistence.clone(),
            joined.runtime.supervisor.binding.clone(),
            joined.runtime.supervisor.shared.clone(),
            joined.runtime.supervisor.lifecycle.clone(),
            cancel.clone(),
            AgentModeDto::Cooperative,
            resume_request.client.clone(),
            joined.runtime.supervisor.heartbeat_gate.clone(),
            "alpha".into(),
            clock.clone(),
            joined.runtime.supervisor.profile_lock(),
        ));
        settle().await;
        assert_eq!(scripted.heartbeat_calls.load(Ordering::SeqCst), 0);
        clock.fire(Duration::from_secs(2)).await;
        assert_eq!(scripted.heartbeat_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            joined.runtime.snapshot().await.heartbeat_interval_seconds,
            5
        );
        assert_eq!(scripted.heartbeat_calls.load(Ordering::SeqCst), 1);
        clock.fire(Duration::from_secs(5)).await;
        assert_eq!(scripted.resume_calls.load(Ordering::SeqCst), 1);
        let rotated_snapshot = joined.runtime.snapshot().await;
        assert_eq!(rotated_snapshot.generation, 1);
        assert_eq!(
            persistence.persisted.lock().unwrap().as_slice(),
            [rotated_snapshot.instance_id.as_str()]
        );
        assert_eq!(scripted.max_in_flight.load(Ordering::SeqCst), 1);
        clock.fire(rotated_cadence).await;
        assert_eq!(
            joined.runtime.snapshot().await.health,
            SessionHealth::OutcomeUnknown
        );
        assert_eq!(scripted.resume_calls.load(Ordering::SeqCst), 1);
        clock.fire(rotated_cadence).await;
        assert_eq!(
            joined.runtime.snapshot().await.health,
            SessionHealth::Degraded
        );
        cancel.cancel();
        tokio::time::timeout(Duration::from_millis(1), scheduler)
            .await
            .unwrap()
            .unwrap();
        let before_failure = joined.runtime.snapshot().await;
        let failure_clock = Arc::new(ManualClock::default());
        let failure_transport = Arc::new(ScriptedTransport {
            heartbeats: TokioMutex::new(VecDeque::from([HeartbeatStep::Return(Err(
                ClientError::Api {
                    status: 409,
                    code: ApiErrorCode::LeaseExpired,
                    retryable: false,
                },
            ))])),
            resumes: TokioMutex::new(VecDeque::from([ResumeStep::Return(Box::new(Ok(
                failed_rotation,
            )))])),
            heartbeat_calls: AtomicUsize::new(0),
            resume_calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            join_block: None,
        });
        let failing_store = Arc::new(ScriptedStore {
            fail_at: AtomicUsize::new(1),
            calls: AtomicUsize::new(0),
            persisted: StdMutex::new(Vec::new()),
        });
        let failure_cancel = CancellationToken::new();
        let failure_scheduler = tokio::spawn(run_heartbeat(
            failure_transport,
            failing_store,
            joined.runtime.supervisor.binding.clone(),
            joined.runtime.supervisor.shared.clone(),
            joined.runtime.supervisor.lifecycle.clone(),
            failure_cancel.clone(),
            AgentModeDto::Cooperative,
            resume_request.client.clone(),
            joined.runtime.supervisor.heartbeat_gate.clone(),
            "alpha".into(),
            failure_clock.clone(),
            joined.runtime.supervisor.profile_lock(),
        ));
        failure_clock.fire(rotated_cadence).await;
        let after_failure = joined.runtime.snapshot().await;
        assert_eq!(after_failure.health, SessionHealth::RotationFailed);
        assert_eq!(after_failure.generation, before_failure.generation);
        assert_eq!(after_failure.instance_id, before_failure.instance_id);
        failure_cancel.cancel();
        failure_scheduler.await.unwrap();
        *joined.runtime.supervisor.lifecycle.write().await = LifecycleState::Ready;
        joined.runtime.supervisor.shared.write().await.health = SessionHealth::Ready;

        let blocked_clock = Arc::new(ManualClock::default());
        let blocked_transport = Arc::new(ScriptedTransport {
            heartbeats: TokioMutex::new(VecDeque::from([HeartbeatStep::Block(blocker)])),
            resumes: TokioMutex::new(VecDeque::new()),
            heartbeat_calls: AtomicUsize::new(0),
            resume_calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            join_block: None,
        });
        let blocked_cancel = CancellationToken::new();
        let blocked_scheduler = tokio::spawn(run_heartbeat(
            blocked_transport.clone(),
            persistence,
            joined.runtime.supervisor.binding.clone(),
            joined.runtime.supervisor.shared.clone(),
            joined.runtime.supervisor.lifecycle.clone(),
            blocked_cancel.clone(),
            AgentModeDto::Cooperative,
            resume_request.client,
            joined.runtime.supervisor.heartbeat_gate.clone(),
            "alpha".into(),
            blocked_clock.clone(),
            joined.runtime.supervisor.profile_lock(),
        ));
        blocked_clock.fire(rotated_cadence).await;
        assert_eq!(blocked_transport.in_flight.load(Ordering::SeqCst), 1);
        blocked_cancel.cancel();
        tokio::time::timeout(Duration::from_millis(10), blocked_scheduler)
            .await
            .unwrap()
            .unwrap();

        let resume_blocker = Arc::new(Notify::new());
        let resume_clock = Arc::new(ManualClock::default());
        let resume_transport = Arc::new(ScriptedTransport {
            heartbeats: TokioMutex::new(VecDeque::from([HeartbeatStep::Return(Err(
                ClientError::Api {
                    status: 409,
                    code: ApiErrorCode::LeaseExpired,
                    retryable: false,
                },
            ))])),
            resumes: TokioMutex::new(VecDeque::from([ResumeStep::Block(resume_blocker.clone())])),
            heartbeat_calls: AtomicUsize::new(0),
            resume_calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            join_block: None,
        });
        let resume_cancel = CancellationToken::new();
        let resume_scheduler = tokio::spawn(run_heartbeat(
            resume_transport.clone(),
            Arc::new(ScriptedStore {
                fail_at: AtomicUsize::new(usize::MAX),
                calls: AtomicUsize::new(0),
                persisted: StdMutex::new(Vec::new()),
            }),
            joined.runtime.supervisor.binding.clone(),
            joined.runtime.supervisor.shared.clone(),
            joined.runtime.supervisor.lifecycle.clone(),
            resume_cancel.clone(),
            AgentModeDto::Cooperative,
            ClientMetadata {
                kind: "test".into(),
                hostname: None,
                version: None,
            },
            joined.runtime.supervisor.heartbeat_gate.clone(),
            "alpha".into(),
            resume_clock.clone(),
            joined.runtime.supervisor.profile_lock(),
        ));
        resume_clock.fire(rotated_cadence).await;
        assert_eq!(resume_transport.resume_calls.load(Ordering::SeqCst), 1);
        resume_cancel.cancel();
        let mut resume_scheduler = resume_scheduler;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut resume_scheduler)
                .await
                .is_err()
        );
        resume_blocker.notify_one();
        tokio::time::timeout(Duration::from_millis(10), resume_scheduler)
            .await
            .unwrap()
            .unwrap();

        let backoff_clock = Arc::new(ManualClock::default());
        let backoff_transport = Arc::new(ScriptedTransport {
            heartbeats: TokioMutex::new(VecDeque::from([
                HeartbeatStep::Return(Err(ClientError::Timeout)),
                HeartbeatStep::Return(Err(ClientError::Timeout)),
                HeartbeatStep::Return(Err(ClientError::Timeout)),
            ])),
            resumes: TokioMutex::new(VecDeque::new()),
            heartbeat_calls: AtomicUsize::new(0),
            resume_calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            join_block: None,
        });
        let backoff_cancel = CancellationToken::new();
        let backoff_scheduler = tokio::spawn(run_heartbeat(
            backoff_transport,
            Arc::new(ScriptedStore {
                fail_at: AtomicUsize::new(usize::MAX),
                calls: AtomicUsize::new(0),
                persisted: StdMutex::new(Vec::new()),
            }),
            joined.runtime.supervisor.binding.clone(),
            joined.runtime.supervisor.shared.clone(),
            joined.runtime.supervisor.lifecycle.clone(),
            backoff_cancel.clone(),
            AgentModeDto::Cooperative,
            ClientMetadata {
                kind: "test".into(),
                hostname: None,
                version: None,
            },
            joined.runtime.supervisor.heartbeat_gate.clone(),
            "alpha".into(),
            backoff_clock.clone(),
            joined.runtime.supervisor.profile_lock(),
        ));
        for backoff in [1_u64, 2, 4] {
            backoff_clock.fire(rotated_cadence).await;
            backoff_clock.fire(Duration::from_secs(backoff)).await;
        }
        backoff_cancel.cancel();
        backoff_scheduler.await.unwrap();
        assert_eq!(old.generation, 0);

        let immediate_blocker = Arc::new(Notify::new());
        let immediate_transport = Arc::new(ScriptedTransport {
            heartbeats: TokioMutex::new(VecDeque::from([HeartbeatStep::Return(Err(
                ClientError::Api {
                    status: 409,
                    code: ApiErrorCode::LeaseExpired,
                    retryable: false,
                },
            ))])),
            resumes: TokioMutex::new(VecDeque::from([ResumeStep::BlockReturn(
                immediate_blocker.clone(),
                Box::new(Ok(immediate_rotation)),
            )])),
            heartbeat_calls: AtomicUsize::new(0),
            resume_calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            join_block: None,
        });
        joined.runtime.supervisor_mut().transport = immediate_transport.clone();
        let old_generation = joined.runtime.snapshot().await.generation;
        let mut report = Box::pin(joined.runtime.report_availability(AvailabilityDto::Unknown));
        tokio::select! {
            result = &mut report => panic!("report completed before release: {result:?}"),
            () = async {
                for _ in 0..100 {
                    if immediate_transport.resume_calls.load(Ordering::SeqCst) == 1 {
                        return;
                    }
                    tokio::task::yield_now().await;
                }
                panic!("resume was not dispatched");
            } => {}
        }
        assert_eq!(immediate_transport.resume_calls.load(Ordering::SeqCst), 1);
        drop(report);
        immediate_blocker.notify_one();
        for _ in 0..100 {
            if joined.runtime.snapshot().await.generation == old_generation + 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        let recovered = joined.runtime.snapshot().await;
        assert_eq!(recovered.generation, old_generation + 1);
        assert_eq!(recovered.availability, AvailabilityDto::Unknown);
        assert_eq!(
            recovered.availability_source,
            AvailabilitySourceDto::Unknown
        );
        assert_eq!(
            *joined.runtime.supervisor.lifecycle.read().await,
            LifecycleState::Ready
        );
        assert_eq!(
            joined
                .runtime
                .authority()
                .await
                .unwrap()
                .credential
                .instance_id(),
            recovered.instance_id
        );
        assert_eq!(immediate_transport.resume_calls.load(Ordering::SeqCst), 1);

        let ordinary_blocker = Arc::new(Notify::new());
        let ordinary_transport = Arc::new(ScriptedTransport {
            heartbeats: TokioMutex::new(VecDeque::from([HeartbeatStep::BlockReturn(
                ordinary_blocker.clone(),
                Ok(psst_protocol::HeartbeatResponse {
                    lease_expires_at: recovered.lease_expires_at,
                    heartbeat_interval_seconds: recovered.heartbeat_interval_seconds,
                }),
            )])),
            resumes: TokioMutex::new(VecDeque::new()),
            heartbeat_calls: AtomicUsize::new(0),
            resume_calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            join_block: None,
        });
        joined.runtime.supervisor_mut().transport = ordinary_transport.clone();
        let mut ordinary = Box::pin(joined.runtime.report_availability(AvailabilityDto::Busy));
        tokio::select! {
            result = &mut ordinary => panic!("ordinary report completed before release: {result:?}"),
            () = async {
                loop {
                    if ordinary_transport.heartbeat_calls.load(Ordering::SeqCst) == 1 {
                        return;
                    }
                    tokio::task::yield_now().await;
                }
            } => {}
        }
        drop(ordinary);
        ordinary_blocker.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if joined.runtime.snapshot().await.availability == AvailabilityDto::Busy {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned ordinary report did not publish relay-confirmed availability");
        let ordinary_snapshot = joined.runtime.snapshot().await;
        assert_eq!(ordinary_snapshot.availability, AvailabilityDto::Busy);
        assert_eq!(
            ordinary_snapshot.availability_source,
            AvailabilitySourceDto::AgentReported
        );
        assert_eq!(ordinary_snapshot.health, SessionHealth::Ready);

        for (error, health) in [
            (ClientError::Timeout, SessionHealth::Degraded),
            (ClientError::OutcomeUnknown, SessionHealth::OutcomeUnknown),
        ] {
            let failing = Arc::new(ScriptedTransport {
                heartbeats: TokioMutex::new(VecDeque::from([HeartbeatStep::Return(Err(error))])),
                resumes: TokioMutex::new(VecDeque::new()),
                heartbeat_calls: AtomicUsize::new(0),
                resume_calls: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                join_block: None,
            });
            joined.runtime.supervisor_mut().transport = failing;
            assert!(matches!(
                joined
                    .runtime
                    .report_availability(AvailabilityDto::Idle)
                    .await,
                Err(SessionError::Relay(_))
            ));
            assert_eq!(joined.runtime.snapshot().await.health, health);
            assert!(matches!(
                joined.runtime.authority().await,
                Err(SessionError::NotReady)
            ));
            joined.runtime.supervisor.shared.write().await.health = SessionHealth::Ready;
        }

        let drop_clock = Arc::new(ManualClock::default());
        let drop_transport = Arc::new(ScriptedTransport {
            heartbeats: TokioMutex::new(VecDeque::from([HeartbeatStep::Return(Err(
                ClientError::Timeout,
            ))])),
            resumes: TokioMutex::new(VecDeque::new()),
            heartbeat_calls: AtomicUsize::new(0),
            resume_calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            join_block: None,
        });
        let drop_cancel = CancellationToken::new();
        joined.runtime.supervisor_mut().cancel = drop_cancel.clone();
        *joined.runtime.supervisor.task.lock().unwrap() = Some(tokio::spawn(run_heartbeat(
            drop_transport.clone(),
            Arc::new(ScriptedStore {
                fail_at: AtomicUsize::new(usize::MAX),
                calls: AtomicUsize::new(0),
                persisted: StdMutex::new(Vec::new()),
            }),
            joined.runtime.supervisor.binding.clone(),
            joined.runtime.supervisor.shared.clone(),
            joined.runtime.supervisor.lifecycle.clone(),
            drop_cancel,
            AgentModeDto::Cooperative,
            ClientMetadata {
                kind: "test".into(),
                hostname: None,
                version: None,
            },
            joined.runtime.supervisor.heartbeat_gate.clone(),
            "alpha".into(),
            drop_clock.clone(),
            joined.runtime.supervisor.profile_lock(),
        )));
        settle().await;
        let lock_path = joined
            .runtime
            .supervisor
            .metadata_path
            .parent()
            .unwrap()
            .join("runtime/profile.lock");
        assert_eq!(Arc::strong_count(&joined.runtime.supervisor), 1);
        let supervisor = Arc::downgrade(&joined.runtime.supervisor);
        drop(joined.runtime);
        drop_clock.fire(rotated_cadence).await;
        assert_eq!(drop_transport.heartbeat_calls.load(Ordering::SeqCst), 0);
        assert!(supervisor.upgrade().is_none());
        assert!(ProfileLock::acquire(&lock_path).is_ok());

        let leave_paths = ProfilePaths {
            metadata: temp.path().join("leave-profile.json"),
            credential: temp.path().join("leave-credential.json"),
            lock: temp.path().join("leave-runtime/profile.lock"),
        };
        let mut leaving = SessionRuntime::join_and_bind(
            client.clone(),
            UnboundRuntimeSpec {
                relay_origin: origin.clone(),
                profile_name: "leave-test".into(),
                squad: "leave-squad".into(),
                paths: leave_paths.clone(),
                shutdown_bound: Duration::from_secs(1),
            },
            JoinSquadRequest {
                name: "leaver".into(),
                role: "test".into(),
                mode: AgentModeDto::Cooperative,
                client: ClientMetadata {
                    kind: "test".into(),
                    hostname: None,
                    version: None,
                },
                mission: Some("dropped leave waiter fixture".into()),
            },
        )
        .await
        .unwrap();
        let leave_dispatched = Arc::new(Notify::new());
        let leave_release = Arc::new(Notify::new());
        let leave_transport = Arc::new(LeaveBarrierTransport {
            inner: leaving.runtime.supervisor.transport.clone(),
            dispatched: leave_dispatched.clone(),
            release: leave_release.clone(),
        });
        leaving.runtime.supervisor_mut().transport = leave_transport;
        let leaving = Arc::new(leaving.runtime);
        let leave_supervisor = leaving.supervisor.clone();
        let mut leave = Box::pin(leaving.leave());
        tokio::select! {
            result = &mut leave => panic!("leave completed before release: {result:?}"),
            () = leave_dispatched.notified() => {}
        }
        assert_eq!(
            *leave_supervisor.lifecycle.read().await,
            LifecycleState::Leaving
        );
        drop(leave);
        let leave_lock = leave_paths.lock.clone();
        let shutdown_runtime = leaving.clone();
        let shutdown_task = tokio::spawn(async move { shutdown_runtime.shutdown().await });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if leave_supervisor
                    .shutdown_gate_waiting
                    .load(Ordering::Acquire)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shutdown did not reach the operation-gate drain");
        assert!(!shutdown_task.is_finished());
        assert!(
            ProfileLock::acquire(&leave_lock).is_err(),
            "shutdown released profile ownership while the old leave owner was blocked"
        );
        assert!(matches!(
            leaving.authority().await,
            Err(SessionError::NotReady)
        ));
        leave_release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if *leave_supervisor.lifecycle.read().await == LifecycleState::Left {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned leave did not publish its terminal state");
        shutdown_task.await.unwrap().unwrap();
        assert_eq!(
            *leaving.supervisor.lifecycle.read().await,
            LifecycleState::Left
        );
        assert!(leaving.supervisor.task.lock().unwrap().is_none());
        assert!(!leave_paths.metadata.exists() && !leave_paths.credential.exists());
        assert!(matches!(
            leaving.authority().await,
            Err(SessionError::NotReady)
        ));
        drop(leaving);
        drop(leave_supervisor);
        let new_owner = ProfileLock::acquire(&leave_lock).unwrap();
        std::fs::write(&leave_paths.metadata, b"new-authority-sentinel").unwrap();
        tokio::task::yield_now().await;
        assert_eq!(
            std::fs::read(&leave_paths.metadata).unwrap(),
            b"new-authority-sentinel"
        );
        drop(new_owner);

        for terminal in [false, true] {
            let label = if terminal { "terminal" } else { "expired" };
            let paths = ProfilePaths {
                metadata: temp.path().join(format!("startup-{label}/profile.json")),
                credential: temp.path().join(format!("startup-{label}/credential.json")),
                lock: temp.path().join(format!("startup-{label}/runtime.lock")),
            };
            let runtime = SessionRuntime::join_and_bind(
                client.clone(),
                UnboundRuntimeSpec {
                    relay_origin: origin.clone(),
                    profile_name: format!("startup-{label}"),
                    squad: format!("startup-{label}-squad"),
                    paths: paths.clone(),
                    shutdown_bound: Duration::from_secs(1),
                },
                JoinSquadRequest {
                    name: format!("startup-{label}-worker"),
                    role: "test".into(),
                    mode: AgentModeDto::Cooperative,
                    client: ClientMetadata {
                        kind: "test".into(),
                        hostname: None,
                        version: None,
                    },
                    mission: Some("startup reconciliation test".into()),
                },
            )
            .await
            .unwrap()
            .runtime;
            let profile = runtime.supervisor.profile.clone();
            let intent = fixed_issuer().intent(&profile).unwrap();
            runtime.supervisor.journal.store(&intent).unwrap();
            if terminal {
                let authority = runtime.authority().await.unwrap();
                client
                    .leave(&profile.squad_name, &authority.credential)
                    .await
                    .unwrap();
            }
            runtime.shutdown().await.unwrap();

            let spec = RuntimeSpec {
                profile: profile.clone(),
                paths: paths.clone(),
                mode: AgentModeDto::Cooperative,
                client_metadata: ClientMetadata {
                    kind: "test".into(),
                    hostname: None,
                    version: None,
                },
                shutdown_bound: Duration::from_secs(1),
            };
            if terminal {
                assert!(matches!(
                    SessionRuntime::start(client.clone(), spec).await,
                    Err(SessionError::Unbound)
                ));
                assert!(!paths.metadata.exists() && !paths.credential.exists());
            } else {
                let mut rotated = client
                    .join(
                        "startup-expired-rotation",
                        &JoinSquadRequest {
                            name: "startup-expired-rotated-worker".into(),
                            role: "test".into(),
                            mode: AgentModeDto::Cooperative,
                            client: ClientMetadata {
                                kind: "test".into(),
                                hostname: None,
                                version: None,
                            },
                            mission: Some("startup rotation fixture".into()),
                        },
                    )
                    .await
                    .unwrap();
                rotated.response.squad.id = profile.squad_id.clone();
                rotated.response.squad.name = profile.squad_name.clone();
                rotated.response.membership_id = profile.member_id.clone();
                let transport = Arc::new(FirstHeartbeatTransport {
                    inner: client.clone(),
                    first: StdMutex::new(Some(ClientError::Api {
                        status: 409,
                        code: ApiErrorCode::LeaseExpired,
                        retryable: false,
                    })),
                    resume: TokioMutex::new(Some(rotated)),
                    heartbeat_calls: AtomicUsize::new(0),
                    resume_calls: AtomicUsize::new(0),
                });
                let recovered = SessionRuntime::start_locked_with(
                    client.clone(),
                    transport.clone(),
                    spec,
                    ProfileLock::acquire(&paths.lock).unwrap(),
                    fixed_issuer(),
                    Arc::new(NoCleanupFault),
                )
                .await
                .unwrap();
                assert_eq!(transport.heartbeat_calls.load(Ordering::SeqCst), 1);
                assert_eq!(transport.resume_calls.load(Ordering::SeqCst), 1);
                assert_eq!(recovered.snapshot().await.generation, 1);
                assert!(
                    !crate::leave_journal::sibling_path(&paths.metadata)
                        .unwrap()
                        .exists()
                );
                recovered.shutdown().await.unwrap();
            }
        }

        for (index, error, expected_health) in [
            (
                0,
                ClientError::Api {
                    status: 400,
                    code: ApiErrorCode::InvalidRequest,
                    retryable: false,
                },
                SessionHealth::Ready,
            ),
            (1, ClientError::Timeout, SessionHealth::OutcomeUnknown),
            (
                2,
                ClientError::Api {
                    status: 400,
                    code: ApiErrorCode::InvalidRequest,
                    retryable: false,
                },
                SessionHealth::Ready,
            ),
        ] {
            let paths = ProfilePaths {
                metadata: temp
                    .path()
                    .join(format!("leave-error-{index}/profile.json")),
                credential: temp
                    .path()
                    .join(format!("leave-error-{index}/credential.json")),
                lock: temp
                    .path()
                    .join(format!("leave-error-{index}/runtime.lock")),
            };
            let mut runtime = SessionRuntime::join_and_bind(
                client.clone(),
                UnboundRuntimeSpec {
                    relay_origin: origin.clone(),
                    profile_name: format!("leave-error-{index}"),
                    squad: format!("leave-error-squad-{index}"),
                    paths: paths.clone(),
                    shutdown_bound: Duration::from_secs(1),
                },
                JoinSquadRequest {
                    name: format!("leave-error-worker-{index}"),
                    role: "test".into(),
                    mode: AgentModeDto::Cooperative,
                    client: ClientMetadata {
                        kind: "test".into(),
                        hostname: None,
                        version: None,
                    },
                    mission: Some("leave error classification test".into()),
                },
            )
            .await
            .unwrap()
            .runtime;
            let original_transport = runtime.supervisor.transport.clone();
            runtime.supervisor_mut().journal_issuer = fixed_issuer();
            let error_transport: Arc<dyn SessionTransport> = Arc::new(LeaveErrorTransport {
                inner: original_transport.clone(),
                error: StdMutex::new(Some(error)),
            });
            if index == 0 {
                let dispatched = Arc::new(Notify::new());
                let release = Arc::new(Notify::new());
                runtime.supervisor_mut().transport = Arc::new(LeaveBarrierTransport {
                    inner: error_transport,
                    dispatched: dispatched.clone(),
                    release: release.clone(),
                });
                let supervisor = runtime.supervisor.clone();
                let mut leave = Box::pin(runtime.leave());
                tokio::select! {
                    result = &mut leave => panic!("leave completed before barrier: {result:?}"),
                    () = dispatched.notified() => {}
                }
                supervisor.request_shutdown();
                release.notify_one();
                assert!(matches!(leave.await, Err(SessionError::Relay(_))));
            } else {
                runtime.supervisor_mut().transport = error_transport;
                assert!(matches!(runtime.leave().await, Err(SessionError::Relay(_))));
            }
            assert_eq!(runtime.snapshot().await.health, expected_health);
            let journal_path = crate::leave_journal::sibling_path(&paths.metadata).unwrap();
            if index == 0 {
                assert!(!journal_path.exists());
                assert!(matches!(
                    runtime.authority().await,
                    Err(SessionError::NotReady)
                ));
                assert!(
                    !runtime
                        .supervisor
                        .sends
                        .admission_open
                        .load(Ordering::Acquire)
                );
                assert!(
                    runtime
                        .supervisor
                        .sends
                        .permanently_closed
                        .load(Ordering::Acquire)
                );
                assert!(
                    runtime
                        .supervisor
                        .read_cancel
                        .lock()
                        .unwrap()
                        .is_cancelled()
                );
                assert!(matches!(
                    runtime
                        .supervisor
                        .sends
                        .reserve(&prepared("after-shutdown".into()))
                        .await,
                    Err(SessionError::NotReady)
                ));
                continue;
            } else if index == 1 {
                assert!(journal_path.exists());
                assert!(matches!(
                    runtime.authority().await,
                    Err(SessionError::NotReady)
                ));
                let profile = runtime.supervisor.profile.clone();
                drop(runtime);
                let failing_transport = Arc::new(FirstHeartbeatTransport {
                    inner: client.clone(),
                    first: StdMutex::new(Some(ClientError::Timeout)),
                    resume: TokioMutex::new(None),
                    heartbeat_calls: AtomicUsize::new(0),
                    resume_calls: AtomicUsize::new(0),
                });
                assert!(matches!(
                    SessionRuntime::start_locked_with(
                        client.clone(),
                        failing_transport,
                        RuntimeSpec {
                            profile: profile.clone(),
                            paths: paths.clone(),
                            mode: AgentModeDto::Cooperative,
                            client_metadata: ClientMetadata {
                                kind: "test".into(),
                                hostname: None,
                                version: None,
                            },
                            shutdown_bound: Duration::from_secs(1),
                        },
                        acquire_lock(&paths.lock).await,
                        fixed_issuer(),
                        Arc::new(NoCleanupFault),
                    )
                    .await,
                    Err(SessionError::Relay(ClientError::Timeout))
                ));
                assert!(journal_path.exists());
                runtime = SessionRuntime::start(
                    client.clone(),
                    RuntimeSpec {
                        profile,
                        paths: paths.clone(),
                        mode: AgentModeDto::Cooperative,
                        client_metadata: ClientMetadata {
                            kind: "test".into(),
                            hostname: None,
                            version: None,
                        },
                        shutdown_bound: Duration::from_secs(1),
                    },
                )
                .await
                .unwrap();
                assert!(!journal_path.exists());
                assert!(runtime.authority().await.is_ok());
            } else {
                assert!(!journal_path.exists());
                assert!(runtime.authority().await.is_ok());
                assert!(
                    runtime
                        .supervisor
                        .sends
                        .admission_open
                        .load(Ordering::Acquire)
                );
                assert!(
                    !runtime
                        .supervisor
                        .read_cancel
                        .lock()
                        .unwrap()
                        .is_cancelled()
                );
                runtime.supervisor_mut().journal_issuer = Arc::new(FailingJournalIssuer);
                assert!(matches!(runtime.leave().await, Err(SessionError::Local(_))));
                assert!(runtime.authority().await.is_ok());
                assert!(
                    runtime
                        .supervisor
                        .sends
                        .admission_open
                        .load(Ordering::Acquire)
                );
                assert!(
                    !runtime
                        .supervisor
                        .read_cancel
                        .lock()
                        .unwrap()
                        .is_cancelled()
                );
                assert!(!journal_path.exists());
                runtime.supervisor_mut().journal_issuer = fixed_issuer();
                runtime.supervisor_mut().transport = original_transport;
            }
            runtime.leave().await.unwrap();
        }

        for (index, step) in [
            CleanupStep::Credential,
            CleanupStep::Profile,
            CleanupStep::Journal,
        ]
        .into_iter()
        .enumerate()
        {
            let fault_paths = ProfilePaths {
                metadata: temp.path().join(format!("cleanup-{index}/profile.json")),
                credential: temp.path().join(format!("cleanup-{index}/credential.json")),
                lock: temp.path().join(format!("cleanup-{index}/runtime.lock")),
            };
            let mut faulted = SessionRuntime::join_and_bind(
                client.clone(),
                UnboundRuntimeSpec {
                    relay_origin: origin.clone(),
                    profile_name: format!("cleanup-{index}"),
                    squad: format!("cleanup-squad-{index}"),
                    paths: fault_paths.clone(),
                    shutdown_bound: Duration::from_secs(1),
                },
                JoinSquadRequest {
                    name: format!("cleanup-worker-{index}"),
                    role: "test".into(),
                    mode: AgentModeDto::Cooperative,
                    client: ClientMetadata {
                        kind: "test".into(),
                        hostname: None,
                        version: None,
                    },
                    mission: Some("cleanup replay test".into()),
                },
            )
            .await
            .unwrap()
            .runtime;
            let profile = faulted.supervisor.profile.clone();
            faulted.supervisor_mut().journal_issuer = fixed_issuer();
            faulted.supervisor_mut().cleanup_seam = Arc::new(FailCleanupAt {
                step,
                fired: std::sync::atomic::AtomicBool::new(false),
            });
            assert!(matches!(faulted.leave().await, Err(SessionError::Local(_))));
            assert_eq!(
                faulted
                    .supervisor
                    .journal
                    .load(&profile)
                    .unwrap()
                    .unwrap()
                    .phase(),
                LeavePhase::Confirmed
            );
            let ambiguous = Arc::new(AmbiguousLeaveCounter {
                inner: faulted.supervisor.transport.clone(),
                calls: AtomicUsize::new(0),
            });
            faulted.supervisor_mut().transport = ambiguous.clone();
            faulted.supervisor_mut().cleanup_seam = Arc::new(FailCleanupAt {
                step,
                fired: std::sync::atomic::AtomicBool::new(false),
            });
            assert!(matches!(faulted.leave().await, Err(SessionError::Local(_))));
            assert_eq!(ambiguous.calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                faulted
                    .supervisor
                    .journal
                    .load(&profile)
                    .unwrap()
                    .unwrap()
                    .phase(),
                LeavePhase::Confirmed
            );
            drop(faulted);

            let dead_client =
                Arc::new(Client::new("http://127.0.0.1:9", ClientConfig::default()).unwrap());
            let replay_fault = SessionRuntime::start_locked_with(
                dead_client.clone(),
                dead_client.clone(),
                RuntimeSpec {
                    profile: profile.clone(),
                    paths: fault_paths.clone(),
                    mode: AgentModeDto::Cooperative,
                    client_metadata: ClientMetadata {
                        kind: "test".into(),
                        hostname: None,
                        version: None,
                    },
                    shutdown_bound: Duration::from_secs(1),
                },
                acquire_lock(&fault_paths.lock).await,
                fixed_issuer(),
                Arc::new(FailCleanupAt {
                    step,
                    fired: std::sync::atomic::AtomicBool::new(false),
                }),
            )
            .await;
            assert!(matches!(replay_fault, Err(SessionError::Local(_))));
            assert!(
                crate::leave_journal::sibling_path(&fault_paths.metadata)
                    .unwrap()
                    .exists()
            );
            if fault_paths.metadata.exists() {
                let recovery_client =
                    Arc::new(Client::new(&profile.relay_origin, ClientConfig::default()).unwrap());
                let recovered = SessionRuntime::start(
                    recovery_client,
                    RuntimeSpec {
                        profile,
                        paths: fault_paths.clone(),
                        mode: AgentModeDto::Cooperative,
                        client_metadata: ClientMetadata {
                            kind: "test".into(),
                            hostname: None,
                            version: None,
                        },
                        shutdown_bound: Duration::from_secs(1),
                    },
                )
                .await;
                assert!(matches!(recovered, Err(SessionError::Unbound)));
            } else {
                assert!(
                    SessionRuntime::recover_orphaned_leave(
                        fault_paths.clone(),
                        profile.relay_origin.clone(),
                        profile.profile.clone(),
                    )
                    .await
                    .unwrap()
                );
            }
            assert!(!fault_paths.metadata.exists());
            assert!(!fault_paths.credential.exists());
            assert!(
                !crate::leave_journal::sibling_path(&fault_paths.metadata)
                    .unwrap()
                    .exists()
            );
        }

        let leave_identity_paths = ProfilePaths {
            metadata: temp.path().join("leave-identity/profile.json"),
            credential: temp.path().join("leave-identity/credential.json"),
            lock: temp.path().join("leave-identity/runtime.lock"),
        };
        let mut leave_identity = SessionRuntime::join_and_bind(
            client.clone(),
            UnboundRuntimeSpec {
                relay_origin: origin.clone(),
                profile_name: "leave-identity".into(),
                squad: "leave-identity-squad".into(),
                paths: leave_identity_paths.clone(),
                shutdown_bound: Duration::from_secs(1),
            },
            JoinSquadRequest {
                name: "leave-identity-worker".into(),
                role: "test".into(),
                mode: AgentModeDto::Cooperative,
                client: ClientMetadata {
                    kind: "test".into(),
                    hostname: None,
                    version: None,
                },
                mission: Some("leave identity validation".into()),
            },
        )
        .await
        .unwrap()
        .runtime;
        let leave_inner = leave_identity.supervisor.transport.clone();
        leave_identity.supervisor_mut().transport = Arc::new(IdentityMismatchTransport {
            inner: leave_inner,
            mismatch_join: false,
            mismatch_leave: true,
        });
        assert!(matches!(
            leave_identity.leave().await,
            Err(SessionError::Relay(ClientError::MalformedResponse {
                status: 200
            }))
        ));
        assert_eq!(
            leave_identity.snapshot().await.health,
            SessionHealth::OutcomeUnknown
        );
        assert!(leave_identity_paths.metadata.exists() && leave_identity_paths.credential.exists());
        assert_eq!(
            leave_identity
                .supervisor
                .journal
                .load(&leave_identity.supervisor.profile)
                .unwrap()
                .unwrap()
                .phase(),
            LeavePhase::Intent
        );

        let fence_paths = ProfilePaths {
            metadata: temp.path().join("availability-fence/profile.json"),
            credential: temp.path().join("availability-fence/credential.json"),
            lock: temp.path().join("availability-fence/runtime.lock"),
        };
        let mut fenced = SessionRuntime::join_and_bind(
            client.clone(),
            UnboundRuntimeSpec {
                relay_origin: origin,
                profile_name: "availability-fence".into(),
                squad: "availability-fence-squad".into(),
                paths: fence_paths.clone(),
                shutdown_bound: Duration::from_secs(1),
            },
            JoinSquadRequest {
                name: "availability-fence-worker".into(),
                role: "test".into(),
                mode: AgentModeDto::Cooperative,
                client: ClientMetadata {
                    kind: "test".into(),
                    hostname: None,
                    version: None,
                },
                mission: Some("terminal availability fence".into()),
            },
        )
        .await
        .unwrap()
        .runtime;
        let dispatched = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let transport = Arc::new(HeartbeatBarrierTransport {
            inner: fenced.supervisor.transport.clone(),
            dispatched: dispatched.clone(),
            release: release.clone(),
            first: std::sync::atomic::AtomicBool::new(true),
            heartbeat_calls: AtomicUsize::new(0),
        });
        fenced.supervisor_mut().transport = transport.clone();
        let supervisor = fenced.supervisor.clone();
        let report_cancel = supervisor.reports.register().unwrap();
        let report_tracker = supervisor.reports.clone();
        let report_supervisor = supervisor.clone();
        let before_leave = tokio::spawn(async move {
            let result = report_availability_owned(
                report_supervisor,
                AvailabilityDto::Busy,
                AvailabilitySourceDto::AgentReported,
                report_cancel,
            )
            .await;
            report_tracker.finish();
            result
        });
        dispatched.notified().await;
        tokio::time::timeout(Duration::from_secs(2), fenced.leave())
            .await
            .expect("leave did not cancel and drain the blocked availability report")
            .unwrap();
        assert!(matches!(
            before_leave.await.unwrap(),
            Err(SessionError::NotReady)
        ));
        assert_eq!(transport.heartbeat_calls.load(Ordering::SeqCst), 1);
        assert_eq!(*supervisor.lifecycle.read().await, LifecycleState::Left);
        assert_eq!(
            supervisor.shared.read().await.health,
            SessionHealth::Stopped
        );
        assert!(!fence_paths.metadata.exists() && !fence_paths.credential.exists());
        assert!(
            !crate::leave_journal::sibling_path(&fence_paths.metadata)
                .unwrap()
                .exists()
        );

        let resume_paths = ProfilePaths {
            metadata: temp.path().join("resume-fence/profile.json"),
            credential: temp.path().join("resume-fence/credential.json"),
            lock: temp.path().join("resume-fence/runtime.lock"),
        };
        let mut resume_fenced = SessionRuntime::join_and_bind(
            client.clone(),
            UnboundRuntimeSpec {
                relay_origin: format!("http://{address}"),
                profile_name: "resume-fence".into(),
                squad: "resume-fence-squad".into(),
                paths: resume_paths,
                shutdown_bound: Duration::from_secs(1),
            },
            JoinSquadRequest {
                name: "resume-fence-worker".into(),
                role: "test".into(),
                mode: AgentModeDto::Cooperative,
                client: ClientMetadata {
                    kind: "test".into(),
                    hostname: None,
                    version: None,
                },
                mission: Some("resume failure leave fence".into()),
            },
        )
        .await
        .unwrap()
        .runtime;
        let resume_dispatched = Arc::new(Notify::new());
        let resume_release = Arc::new(Notify::new());
        let resume_transport = Arc::new(ResumeFailureBarrierTransport {
            inner: resume_fenced.supervisor.transport.clone(),
            resume_dispatched: resume_dispatched.clone(),
            resume_release: resume_release.clone(),
            first_heartbeat: std::sync::atomic::AtomicBool::new(true),
            resume_calls: AtomicUsize::new(0),
        });
        resume_fenced.supervisor_mut().transport = resume_transport.clone();
        let resume_supervisor = resume_fenced.supervisor.clone();
        let report_cancel = resume_supervisor.reports.register().unwrap();
        let report_tracker = resume_supervisor.reports.clone();
        let report_supervisor = resume_supervisor.clone();
        let report = tokio::spawn(async move {
            let result = report_availability_owned(
                report_supervisor,
                AvailabilityDto::Busy,
                AvailabilitySourceDto::AgentReported,
                report_cancel,
            )
            .await;
            report_tracker.finish();
            result
        });
        resume_dispatched.notified().await;
        let mut leave = Box::pin(resume_fenced.leave());
        tokio::select! {
            result = &mut leave => panic!("leave bypassed in-flight resume: {result:?}"),
            () = settle() => {}
        }
        assert_eq!(
            *resume_supervisor.lifecycle.read().await,
            LifecycleState::Recovering
        );
        resume_release.notify_one();
        assert!(matches!(
            report.await.unwrap(),
            Err(SessionError::Relay(ClientError::OutcomeUnknown))
        ));
        assert!(matches!(leave.await, Err(SessionError::NotReady)));
        assert_eq!(resume_transport.resume_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *resume_supervisor.lifecycle.read().await,
            LifecycleState::OutcomeUnknown
        );

        shutdown.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    fn prepared(body: String) -> PreparedSend {
        Client::new("http://127.0.0.1:9", ClientConfig::default())
            .unwrap()
            .prepare_send(
                "recipient".to_owned(),
                body,
                psst_protocol::MessagePriorityDto::Normal,
                None,
                None,
            )
            .unwrap()
    }

    fn send_response(request: &PreparedSend, squad: &str, sender: String) -> SendMessageResponse {
        SendMessageResponse {
            message: psst_protocol::MessageDto {
                sequence: MessageSequence::new(1).unwrap(),
                id: "msg_test".into(),
                squad: squad.into(),
                sender,
                recipient: request.request().recipient.clone(),
                body: request.request().body.clone(),
                priority: request.request().priority,
                reply_to: request.request().reply_to.clone(),
                correlation_id: request.request().correlation_id.clone(),
                created_at: serde_json::from_str("\"2026-08-08T01:02:03.004Z\"").unwrap(),
                acknowledged_at: None,
            },
            idempotent_replay: false,
        }
    }

    #[tokio::test]
    async fn send_terminal_rejects_unrelated_response_and_never_exceeds_retained_bound() {
        let ledger = SendLedger::new();
        let request = prepared("expected-body".to_owned());
        let entry = match ledger.reserve(&request).await.unwrap() {
            SendReservation::New(entry) => entry,
            SendReservation::Existing(_) => unreachable!(),
        };
        let mut unrelated = send_response(&request, "alpha", "alice".into());
        unrelated.message.body = "unrelated-body".into();
        assert!(matches!(
            validate_owned_send_response(&request, "alpha", &unrelated),
            Err(StoredSendFailure::Relay(
                StoredClientError::MalformedResponse { status: 200 }
            ))
        ));

        let oversized = send_response(
            &request,
            "alpha",
            "s".repeat(entry.retained_bytes.saturating_add(1)),
        );
        assert!(send_terminal_retained_bytes(&Ok(oversized.clone())) > entry.retained_bytes);
        ledger
            .finish(request.operation_identity(), &entry, Ok(oversized))
            .await;
        let state = ledger.inner.lock().await;
        assert!(!state.entries.contains_key(&request.operation_identity()));
        assert_eq!(state.retained_bytes, 0);
        assert!(state.retained_bytes <= MAX_OWNED_SEND_BYTES);
    }

    #[test]
    fn prepared_send_identity_is_opaque_and_debug_never_contains_body_or_key() {
        let request = prepared("the-secret-body-marker".to_owned());
        let clone = request.clone();
        let distinct = prepared("the-secret-body-marker".to_owned());
        assert_eq!(request.operation_identity(), clone.operation_identity());
        assert_ne!(request.operation_identity(), distinct.operation_identity());
        let debug = format!("{request:?} {:?}", request.operation_identity());
        assert!(!debug.contains("the-secret-body-marker"));
        assert!(!debug.contains(request.request().dedupe_key.as_str()));
        assert!(debug.contains("REDACTED") && debug.contains("OPAQUE"));
    }

    #[tokio::test]
    async fn send_ledger_rejoins_dropped_waiter_without_evicting_inflight() {
        let ledger = Arc::new(SendLedger::new());
        let request = prepared("body".to_owned());
        let entry = match ledger.reserve(&request).await.unwrap() {
            SendReservation::New(entry) => entry,
            SendReservation::Existing(_) => panic!("first reservation must own dispatch"),
        };
        let waiter = tokio::spawn(await_send(entry.clone()));
        waiter.abort();
        let _ = waiter.await;

        assert!(matches!(
            ledger.reserve(&request).await.unwrap(),
            SendReservation::Existing(_)
        ));
        ledger
            .finish(
                request.operation_identity(),
                &entry,
                Err(StoredSendFailure::Relay(StoredClientError::Timeout)),
            )
            .await;
        let rejoined = match ledger.reserve(&request).await.unwrap() {
            SendReservation::Existing(entry) => entry,
            SendReservation::New(_) => panic!("terminal operation identity must be retained"),
        };
        assert!(matches!(
            await_send(rejoined).await,
            Err(SessionError::Relay(ClientError::Timeout))
        ));
    }

    #[tokio::test]
    async fn send_ledger_bounds_total_retained_bytes_and_never_evicts_inflight() {
        let ledger = SendLedger::new();
        let mut admitted = Vec::new();
        loop {
            let request = prepared("x".repeat(64 * 1024));
            match ledger.reserve(&request).await {
                Ok(SendReservation::New(entry)) => admitted.push((request, entry)),
                Ok(SendReservation::Existing(_)) => unreachable!(),
                Err(SessionError::SendCapacity) => break,
                Err(error) => panic!("unexpected reservation error: {error}"),
            }
        }
        assert!(!admitted.is_empty());
        let distinct = prepared("y".repeat(64 * 1024));
        let (full, entry) = admitted.remove(0);
        ledger
            .finish(
                full.operation_identity(),
                &entry,
                Err(StoredSendFailure::Relay(StoredClientError::Timeout)),
            )
            .await;
        assert!(matches!(
            ledger.reserve(&distinct).await.unwrap(),
            SendReservation::New(_)
        ));
        let state = ledger.inner.lock().await;
        assert!(state.retained_bytes <= MAX_OWNED_SEND_BYTES);
        assert!(state.retained_bytes > distinct.request().body.len());
    }

    #[tokio::test]
    async fn send_ledger_stops_admission_and_bounded_drain_observes_owned_terminal() {
        let ledger = Arc::new(SendLedger::new());
        let request = prepared("body".to_owned());
        let entry = match ledger.reserve(&request).await.unwrap() {
            SendReservation::New(entry) => entry,
            SendReservation::Existing(_) => unreachable!(),
        };
        let draining = tokio::spawn({
            let ledger = ledger.clone();
            async move { ledger.stop_and_drain(Duration::from_secs(1)).await }
        });
        tokio::task::yield_now().await;
        assert!(matches!(
            ledger.reserve(&prepared("later".to_owned())).await,
            Err(SessionError::NotReady)
        ));
        assert!(!draining.is_finished());
        ledger
            .finish(
                request.operation_identity(),
                &entry,
                Err(StoredSendFailure::Relay(StoredClientError::Timeout)),
            )
            .await;
        draining.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn send_ledger_can_reopen_after_recoverable_drain_timeout() {
        let ledger = SendLedger::new();
        let blocked = prepared("blocked".to_owned());
        let blocked_entry = match ledger.reserve(&blocked).await.unwrap() {
            SendReservation::New(entry) => entry,
            SendReservation::Existing(_) => unreachable!(),
        };
        assert!(matches!(
            ledger.stop_and_drain(Duration::from_millis(1)).await,
            Err(SessionError::ShutdownTimedOut)
        ));
        ledger.reopen_admission().await;
        let resumed = prepared("resumed".to_owned());
        assert!(matches!(
            ledger.reserve(&resumed).await.unwrap(),
            SendReservation::New(_)
        ));
        ledger
            .finish(
                blocked.operation_identity(),
                &blocked_entry,
                Err(StoredSendFailure::NotReady),
            )
            .await;
    }

    #[tokio::test]
    async fn canceled_read_epoch_wins_before_roster_inbox_and_transcript_dispatch() {
        for operation in ["roster", "inbox", "transcript"] {
            let epoch = CancellationToken::new();
            let dispatches = Arc::new(AtomicUsize::new(0));
            // Models the deterministic pause after authority capture: leave cancels
            // that captured epoch before the client future receives its first poll.
            epoch.cancel();
            let observed = dispatches.clone();
            let result: Result<(), SessionError> = read_with_epoch(epoch, async move {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await;
            assert!(matches!(result, Err(SessionError::NotReady)), "{operation}");
            assert_eq!(dispatches.load(Ordering::SeqCst), 0, "{operation}");
        }
    }

    #[tokio::test]
    async fn report_tracker_bounds_admission_and_drains_every_admitted_operation() {
        let tracker = Arc::new(ReportTracker::new());
        let mut tokens = Vec::new();
        for _ in 0..MAX_ACTIVE_REPORTS {
            tokens.push(tracker.register().unwrap());
        }
        assert!(matches!(
            tracker.register(),
            Err(SessionError::OperationCapacity)
        ));
        tracker.cancel();
        assert!(tokens.iter().all(CancellationToken::is_cancelled));
        let draining = tokio::spawn({
            let tracker = tracker.clone();
            async move { tracker.drain(Duration::from_secs(1)).await }
        });
        for _ in tokens {
            tracker.finish();
        }
        draining.await.unwrap().unwrap();
        tracker.reopen();
        assert!(tracker.register().is_ok());
        tracker.finish();
        tracker.close_permanently();
        tracker.reopen();
        assert!(matches!(tracker.register(), Err(SessionError::NotReady)));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn dropped_send_waiter_preserves_typed_client_key_and_one_durable_row() {
        let temp = tempfile::tempdir().unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let database = temp.path().join("relay.db");
        let mut relay = psst_relay::RelayConfig::local(database.clone());
        relay.bind = address;
        relay.request_timeout = Duration::from_millis(100);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (probe_tx, probe_rx) = oneshot::channel();
        let server = tokio::spawn(psst_relay::serve_with_reliability_probe(
            relay,
            shutdown_rx,
            probe_tx,
        ));
        let worker = probe_rx.await.unwrap();
        let config = ClientConfig {
            request_timeout: Duration::from_secs(1),
            retry: psst_client::RetryPolicy {
                max_attempts: 5,
                initial_backoff: Duration::from_millis(10),
                max_backoff: Duration::from_millis(20),
            },
            ..ClientConfig::default()
        };
        let client = Arc::new(Client::new(&format!("http://{address}"), config).unwrap());
        for _ in 0..100 {
            if client.health().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let paths = crash_fixture_paths(temp.path());
        let runtime = Arc::new(
            SessionRuntime::join_and_bind(
                client.clone(),
                UnboundRuntimeSpec {
                    relay_origin: format!("http://{address}"),
                    profile_name: "default".into(),
                    squad: "alpha".into(),
                    paths,
                    shutdown_bound: Duration::from_secs(2),
                },
                JoinSquadRequest {
                    name: "alice".into(),
                    role: "sender".into(),
                    mode: AgentModeDto::Cooperative,
                    client: ClientMetadata {
                        kind: "test".into(),
                        hostname: None,
                        version: None,
                    },
                    mission: Some("send ownership".into()),
                },
            )
            .await
            .unwrap()
            .runtime,
        );
        client
            .join(
                "alpha",
                &JoinSquadRequest {
                    name: "bob".into(),
                    role: "recipient".into(),
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
        let committed = worker
            .reliability_delay_next_send_reply(Duration::from_millis(300))
            .await
            .unwrap();
        let request = client
            .prepare_send(
                "bob".to_owned(),
                "commit-before-caller-cancel".to_owned(),
                psst_protocol::MessagePriorityDto::Normal,
                None,
                None,
            )
            .unwrap();
        let waiter = tokio::spawn({
            let runtime = runtime.clone();
            let request = request.clone();
            async move { runtime.send_prepared(&request).await }
        });
        for _ in 0..100 {
            if runtime.state().sends.inner.lock().await.inflight == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(runtime.state().sends.inner.lock().await.inflight, 1);
        waiter.abort();
        let _ = waiter.await;
        tokio::task::spawn_blocking(move || committed.recv_timeout(Duration::from_secs(2)))
            .await
            .unwrap()
            .unwrap();

        let send_result = runtime.send_prepared(&request).await.unwrap();
        assert_eq!(send_result.message.sequence.value(), 1);
        assert!(send_result.idempotent_replay);
        let transcript = runtime
            .transcript(MessageSequence::default(), 100)
            .await
            .unwrap();
        assert_eq!(transcript.messages.len(), 1);
        assert_eq!(transcript.messages[0].id, send_result.message.id);

        let gate = runtime.state().heartbeat_gate.lock().await;
        let fenced = client
            .prepare_send(
                "bob".to_owned(),
                "stale-authority-must-not-dispatch".to_owned(),
                psst_protocol::MessagePriorityDto::Normal,
                None,
                None,
            )
            .unwrap();
        let fenced_waiter = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.send_prepared(&fenced).await }
        });
        for _ in 0..100 {
            if runtime.state().sends.inner.lock().await.inflight == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        let generation = runtime.state().shared.read().await.generation;
        runtime.state().shared.write().await.health = SessionHealth::OutcomeUnknown;
        drop(gate);
        assert!(matches!(
            fenced_waiter.await.unwrap(),
            Err(SessionError::NotReady)
        ));
        assert_eq!(runtime.state().shared.read().await.generation, generation);
        runtime.state().shared.write().await.health = SessionHealth::Ready;
        assert_eq!(
            runtime
                .transcript(MessageSequence::default(), 100)
                .await
                .unwrap()
                .messages
                .len(),
            1
        );

        let committed = worker
            .reliability_delay_next_send_reply(Duration::from_millis(300))
            .await
            .unwrap();
        let second = client
            .prepare_send(
                "bob".to_owned(),
                "leave-drains-owned-send".to_owned(),
                psst_protocol::MessagePriorityDto::Normal,
                None,
                None,
            )
            .unwrap();
        let waiter = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.send_prepared(&second).await }
        });
        for _ in 0..100 {
            if runtime.state().sends.inner.lock().await.inflight == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(runtime.state().sends.inner.lock().await.inflight, 1);
        waiter.abort();
        let _ = waiter.await;
        let runtime = Arc::try_unwrap(runtime).unwrap_or_else(|_| panic!("runtime Arc leaked"));
        let mut leave = Box::pin(runtime.leave());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut leave)
                .await
                .is_err()
        );
        tokio::task::spawn_blocking(move || committed.recv_timeout(Duration::from_secs(2)))
            .await
            .unwrap()
            .unwrap();
        leave.await.unwrap();

        shutdown.send(true).unwrap();
        server.await.unwrap().unwrap();
    }
}
