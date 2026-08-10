use crate::{SessionError, SessionRuntime};
use psst_client::Error as ClientError;
use psst_protocol::{InboxResponse, MessagePriorityDto};
use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::{sync::RwLock, task::JoinHandle};
use tokio_util::sync::CancellationToken;

pub const MAX_WAKE_PENDING_COUNT: u64 = 1_000_000;
pub const MAX_WAKE_PROFILE_BYTES: usize = 64;
pub const MAX_WAKE_SQUAD_BYTES: usize = 64;
pub const MAX_WAKE_MESSAGE_ID_BYTES: usize = 128;
pub const DEFAULT_RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
pub const MIN_RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
pub const MAX_RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
pub const DEFAULT_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
pub const DEFAULT_BACKOFF_MAX: Duration = Duration::from_secs(60);
pub const MAX_BACKOFF_DURATION: Duration = Duration::from_secs(300);
pub const MAX_BACKOFF_ATTEMPTS: u8 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationPhase {
    Quiet,
    Pending,
    Waking,
    Running,
    Backoff,
    Blocked,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationHostKind {
    ClaudeChannel,
    CodexAppServer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationOutcome {
    Accepted,
    Running,
    Completed,
    RetryableFailure,
    PermanentFailure,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostFailure {
    RetryableBeforeStart,
    OutcomeUnknown,
    Permanent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationFailure {
    Retryable,
    Permanent,
}

pub type ActivationFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Relay-backed pending-mail observation. Implementations return `None` only for an empty bounded
/// poll and must never acknowledge messages.
pub trait ActivationSource: Send + Sync {
    fn observe(
        &self,
        maximum_wait: Duration,
    ) -> ActivationFuture<'_, Result<Option<WakeMetadata>, ObservationFailure>>;
}

/// One accepted host turn. Completion is distinct from start so the engine can forbid preemption
/// and distinguish a safe pre-start retry from an ambiguous issued turn.
pub trait ActivationTurn: Send {
    fn completed(self: Box<Self>) -> ActivationFuture<'static, Result<(), HostFailure>>;
}

/// A client-specific boundary that starts one host turn from fixed wake metadata.
pub trait ActivationHost: Send + Sync {
    fn start<'a>(
        &'a self,
        wake: &'a WakeMetadata,
    ) -> ActivationFuture<'a, Result<Box<dyn ActivationTurn>, HostFailure>>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivationCommand {
    None,
    Activate(WakeMetadata),
    ReconcileNow,
    RetryAfter(Duration),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationSnapshot {
    pub phase: ActivationPhase,
    pub pending: Option<WakeMetadata>,
    pub reconcile_needed: bool,
    pub retry_attempt: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivationContractError {
    EmptyProfile,
    InvalidProfile,
    EmptySquad,
    InvalidSquad,
    PendingCountOutOfRange,
    EmptyOldestMessageId,
    InvalidOldestMessageId,
    ReconcileIntervalOutOfRange,
    BackoffRangeInvalid,
    InvalidTransition,
}

impl fmt::Display for ActivationContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyProfile => "wake profile must not be empty",
            Self::InvalidProfile => "wake profile is outside the closed contract",
            Self::EmptySquad => "wake squad must not be empty",
            Self::InvalidSquad => "wake squad is outside the closed contract",
            Self::PendingCountOutOfRange => "wake pending count is outside the closed contract",
            Self::EmptyOldestMessageId => "wake oldest message id must not be empty",
            Self::InvalidOldestMessageId => "wake oldest message id is outside the closed contract",
            Self::ReconcileIntervalOutOfRange => {
                "activation reconciliation interval is outside the closed contract"
            }
            Self::BackoffRangeInvalid => "activation backoff range is invalid",
            Self::InvalidTransition => "activation state transition is invalid",
        })
    }
}

#[derive(Clone, Debug)]
pub struct ActivationMachine {
    policy: ActivationPolicy,
    phase: ActivationPhase,
    pending: Option<WakeMetadata>,
    reconcile_needed: bool,
    retry_attempt: u8,
}

impl ActivationMachine {
    /// Creates a quiet activation machine with validated bounds.
    ///
    /// # Errors
    /// Returns an error when the supplied policy is outside the closed activation contract.
    pub fn new(policy: ActivationPolicy) -> Result<Self, ActivationContractError> {
        Ok(Self {
            policy: policy.validate()?,
            phase: ActivationPhase::Quiet,
            pending: None,
            reconcile_needed: false,
            retry_attempt: 0,
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> ActivationSnapshot {
        ActivationSnapshot {
            phase: self.phase,
            pending: self.pending.clone(),
            reconcile_needed: self.reconcile_needed,
            retry_attempt: self.retry_attempt,
        }
    }

    pub fn observe(&mut self, pending: Option<WakeMetadata>) -> ActivationCommand {
        if matches!(
            self.phase,
            ActivationPhase::Stopped | ActivationPhase::Blocked
        ) {
            return ActivationCommand::None;
        }
        let Some(pending) = pending else {
            return ActivationCommand::None;
        };
        match self.phase {
            ActivationPhase::Quiet => {
                self.phase = ActivationPhase::Pending;
                self.pending = Some(pending.clone());
                ActivationCommand::Activate(pending)
            }
            ActivationPhase::Pending
            | ActivationPhase::Waking
            | ActivationPhase::Running
            | ActivationPhase::Backoff => {
                self.pending = Some(pending);
                self.reconcile_needed = true;
                ActivationCommand::None
            }
            ActivationPhase::Blocked | ActivationPhase::Stopped => ActivationCommand::None,
        }
    }

    /// Records that an activation command is being issued to the host.
    ///
    /// # Errors
    /// Returns an error unless an outstanding wake is pending.
    pub fn wake_started(&mut self) -> Result<(), ActivationContractError> {
        if self.phase != ActivationPhase::Pending || self.pending.is_none() {
            return Err(ActivationContractError::InvalidTransition);
        }
        self.phase = ActivationPhase::Waking;
        Ok(())
    }

    /// Records that the host accepted the wake and began a turn.
    ///
    /// # Errors
    /// Returns an error unless the machine is issuing a wake.
    pub fn wake_accepted(&mut self) -> Result<(), ActivationContractError> {
        if self.phase != ActivationPhase::Waking {
            return Err(ActivationContractError::InvalidTransition);
        }
        self.phase = ActivationPhase::Running;
        Ok(())
    }

    /// Records terminal completion and requires an authoritative inbox reconciliation.
    ///
    /// # Errors
    /// Returns an error unless a host activation is waking or running.
    pub fn wake_completed(&mut self) -> Result<ActivationCommand, ActivationContractError> {
        if !matches!(
            self.phase,
            ActivationPhase::Waking | ActivationPhase::Running
        ) {
            return Err(ActivationContractError::InvalidTransition);
        }
        self.phase = ActivationPhase::Quiet;
        self.pending = None;
        self.reconcile_needed = false;
        self.retry_attempt = 0;
        Ok(ActivationCommand::ReconcileNow)
    }

    /// Records a retryable host failure and returns the unjittered backoff ceiling.
    ///
    /// # Errors
    /// Returns an error unless a host activation is waking or running.
    pub fn wake_retryable_failure(&mut self) -> Result<ActivationCommand, ActivationContractError> {
        if !matches!(
            self.phase,
            ActivationPhase::Waking | ActivationPhase::Running
        ) {
            return Err(ActivationContractError::InvalidTransition);
        }
        self.retry_attempt = self.retry_attempt.saturating_add(1);
        if self.retry_attempt >= self.policy.maximum_attempts {
            self.phase = ActivationPhase::Blocked;
            return Ok(ActivationCommand::None);
        }
        self.phase = ActivationPhase::Backoff;
        let exponent = u32::from(self.retry_attempt.saturating_sub(1));
        let multiplier = 2_u32.checked_pow(exponent).unwrap_or(u32::MAX);
        let delay = self
            .policy
            .initial_backoff
            .checked_mul(multiplier)
            .unwrap_or(self.policy.maximum_backoff)
            .min(self.policy.maximum_backoff);
        Ok(ActivationCommand::RetryAfter(delay))
    }

    /// Moves a due retry back to pending activation.
    ///
    /// # Errors
    /// Returns an error unless the machine is in backoff with retained pending mail.
    pub fn retry_due(&mut self) -> Result<ActivationCommand, ActivationContractError> {
        if self.phase != ActivationPhase::Backoff {
            return Err(ActivationContractError::InvalidTransition);
        }
        let pending = self
            .pending
            .clone()
            .ok_or(ActivationContractError::InvalidTransition)?;
        self.phase = ActivationPhase::Pending;
        self.reconcile_needed = false;
        Ok(ActivationCommand::Activate(pending))
    }

    /// Records a permanent host incompatibility.
    ///
    /// # Errors
    /// Returns an error unless a host activation is waking or running.
    pub fn wake_permanent_failure(&mut self) -> Result<(), ActivationContractError> {
        if !matches!(
            self.phase,
            ActivationPhase::Waking | ActivationPhase::Running
        ) {
            return Err(ActivationContractError::InvalidTransition);
        }
        self.phase = ActivationPhase::Blocked;
        Ok(())
    }

    pub fn stop(&mut self) {
        self.phase = ActivationPhase::Stopped;
        self.pending = None;
        self.reconcile_needed = false;
    }

    fn block(&mut self) {
        if self.phase != ActivationPhase::Stopped {
            self.phase = ActivationPhase::Blocked;
        }
    }
}

pub struct ActivationRuntime {
    machine: Arc<RwLock<ActivationMachine>>,
    cancel: CancellationToken,
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

pub struct SessionActivationSource {
    runtime: Arc<SessionRuntime>,
    profile: String,
    squad: String,
}

impl SessionActivationSource {
    /// Binds activation observation to one already-owned session profile.
    ///
    /// # Errors
    /// Returns an error when profile or squad identifiers are outside the wake contract.
    pub fn new(
        runtime: Arc<SessionRuntime>,
        profile: String,
        squad: String,
    ) -> Result<Self, ActivationContractError> {
        validate_identifier(
            &profile,
            MAX_WAKE_PROFILE_BYTES,
            ActivationContractError::EmptyProfile,
            ActivationContractError::InvalidProfile,
        )?;
        validate_identifier(
            &squad,
            MAX_WAKE_SQUAD_BYTES,
            ActivationContractError::EmptySquad,
            ActivationContractError::InvalidSquad,
        )?;
        Ok(Self {
            runtime,
            profile,
            squad,
        })
    }
}

impl ActivationSource for SessionActivationSource {
    fn observe(
        &self,
        maximum_wait: Duration,
    ) -> ActivationFuture<'_, Result<Option<WakeMetadata>, ObservationFailure>> {
        Box::pin(async move {
            let wait_seconds = u8::try_from(maximum_wait.as_secs().min(30))
                .expect("inbox wait is bounded to 30 seconds");
            let response = self
                .runtime
                .inbox(100, wait_seconds)
                .await
                .map_err(|error| classify_observation_failure(&error))?;
            wake_from_inbox(&self.profile, &self.squad, response)
        })
    }
}

impl ActivationRuntime {
    /// Starts the owned observer/activation loop.
    ///
    /// # Errors
    /// Returns an error when policy bounds are invalid. No task is spawned on error.
    pub fn start(
        source: Arc<dyn ActivationSource>,
        host: Arc<dyn ActivationHost>,
        policy: ActivationPolicy,
    ) -> Result<Self, ActivationContractError> {
        let machine = Arc::new(RwLock::new(ActivationMachine::new(policy)?));
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_activation(
            machine.clone(),
            source,
            host,
            policy,
            cancel.clone(),
        ));
        Ok(Self {
            machine,
            cancel,
            task: tokio::sync::Mutex::new(Some(task)),
        })
    }

    #[must_use]
    pub async fn snapshot(&self) -> ActivationSnapshot {
        self.machine.read().await.snapshot()
    }

    pub async fn shutdown(&self) {
        self.cancel.cancel();
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
        self.machine.write().await.stop();
    }
}

impl Drop for ActivationRuntime {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Ok(task) = self.task.try_lock()
            && let Some(task) = task.as_ref()
        {
            task.abort();
        }
    }
}

async fn run_activation(
    machine: Arc<RwLock<ActivationMachine>>,
    source: Arc<dyn ActivationSource>,
    host: Arc<dyn ActivationHost>,
    policy: ActivationPolicy,
    cancel: CancellationToken,
) {
    let maximum_wait = policy.reconcile_interval.min(Duration::from_secs(30));
    let mut next_wait = maximum_wait;
    let mut observation_failures = 0_u8;
    loop {
        let observed = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            result = source.observe(next_wait) => result,
        };
        next_wait = maximum_wait;
        let pending = match observed {
            Ok(pending) => {
                observation_failures = 0;
                pending
            }
            Err(ObservationFailure::Permanent) => {
                machine.write().await.block();
                break;
            }
            Err(ObservationFailure::Retryable) => {
                observation_failures = observation_failures.saturating_add(1);
                if observation_failures >= policy.maximum_attempts {
                    machine.write().await.block();
                    break;
                }
                if wait_or_cancel(retry_delay(policy, observation_failures), &cancel).await {
                    break;
                }
                continue;
            }
        };
        let command = machine.write().await.observe(pending);
        let ActivationCommand::Activate(wake) = command else {
            continue;
        };
        if drive_wake(&machine, host.as_ref(), wake, &cancel).await {
            break;
        }
        if machine.read().await.snapshot().phase == ActivationPhase::Blocked {
            break;
        }
        // A completed host turn never proves that the mailbox is empty. Reconcile
        // immediately once, then resume bounded long-polling if no mail remains.
        next_wait = Duration::ZERO;
    }
}

async fn drive_wake(
    machine: &Arc<RwLock<ActivationMachine>>,
    host: &dyn ActivationHost,
    mut wake: WakeMetadata,
    cancel: &CancellationToken,
) -> bool {
    loop {
        if machine.write().await.wake_started().is_err() {
            machine.write().await.block();
            return false;
        }
        let started = tokio::select! {
            biased;
            () = cancel.cancelled() => return true,
            result = host.start(&wake) => result,
        };
        let turn = match started {
            Ok(turn) => turn,
            Err(HostFailure::RetryableBeforeStart) => {
                let Ok(command) = machine.write().await.wake_retryable_failure() else {
                    machine.write().await.block();
                    return false;
                };
                let ActivationCommand::RetryAfter(delay) = command else {
                    return false;
                };
                if wait_or_cancel(jitter_delay(delay), cancel).await {
                    return true;
                }
                let Ok(command) = machine.write().await.retry_due() else {
                    machine.write().await.block();
                    return false;
                };
                let ActivationCommand::Activate(retained) = command else {
                    machine.write().await.block();
                    return false;
                };
                wake = retained;
                continue;
            }
            Err(HostFailure::OutcomeUnknown | HostFailure::Permanent) => {
                let _ = machine.write().await.wake_permanent_failure();
                return false;
            }
        };
        if machine.write().await.wake_accepted().is_err() {
            machine.write().await.block();
            return false;
        }
        let completed = tokio::select! {
            biased;
            () = cancel.cancelled() => return true,
            result = turn.completed() => result,
        };
        match completed {
            Ok(()) => {
                let _ = machine.write().await.wake_completed();
                return false;
            }
            Err(
                HostFailure::RetryableBeforeStart
                | HostFailure::OutcomeUnknown
                | HostFailure::Permanent,
            ) => {
                // Once a turn was accepted, every missing completion is ambiguous. Retrying could
                // create a duplicate model turn, so the engine fails closed for host reconciliation.
                let _ = machine.write().await.wake_permanent_failure();
                return false;
            }
        }
    }
}

async fn wait_or_cancel(delay: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => true,
        () = tokio::time::sleep(delay) => false,
    }
}

fn retry_delay(policy: ActivationPolicy, attempt: u8) -> Duration {
    let exponent = u32::from(attempt.saturating_sub(1));
    policy
        .initial_backoff
        .checked_mul(2_u32.checked_pow(exponent).unwrap_or(u32::MAX))
        .unwrap_or(policy.maximum_backoff)
        .min(policy.maximum_backoff)
}

fn jitter_delay(delay: Duration) -> Duration {
    let ceiling_millis = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
    if ceiling_millis < 2 {
        return delay;
    }
    let mut random = [0_u8; 8];
    if psst_platform_security::fill_secure_random(&mut random).is_err() {
        return delay;
    }
    jitter_delay_from(delay, u64::from_le_bytes(random))
}

fn jitter_delay_from(delay: Duration, sample: u64) -> Duration {
    let ceiling_millis = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
    if ceiling_millis < 2 {
        return delay;
    }
    let floor_millis = ceiling_millis / 2;
    let width = ceiling_millis - floor_millis;
    Duration::from_millis(floor_millis + sample % (width + 1))
}

fn wake_from_inbox(
    profile: &str,
    squad: &str,
    response: InboxResponse,
) -> Result<Option<WakeMetadata>, ObservationFailure> {
    if response.pending_count == 0 {
        return Ok(None);
    }
    let highest_priority = response
        .highest_priority
        .ok_or(ObservationFailure::Permanent)?;
    let oldest_message_id = response
        .oldest_message_id
        .ok_or(ObservationFailure::Permanent)?;
    WakeMetadata::new(
        profile.to_owned(),
        squad.to_owned(),
        response.pending_count,
        highest_priority,
        oldest_message_id,
    )
    .map(Some)
    .map_err(|_| ObservationFailure::Permanent)
}

fn classify_observation_failure(error: &SessionError) -> ObservationFailure {
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
        SessionError::Local(_)
        | SessionError::Relay(_)
        | SessionError::ShutdownTimedOut
        | SessionError::Unbound
        | SessionError::RecoveryOutcomeUnknown => ObservationFailure::Permanent,
    }
}

impl std::error::Error for ActivationContractError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WakeMetadata {
    profile: String,
    squad: String,
    pending_count: u64,
    highest_priority: MessagePriorityDto,
    oldest_message_id: String,
}

impl WakeMetadata {
    /// Creates bounded, adapter-controlled wake metadata.
    ///
    /// # Errors
    /// Returns an error when any identifier, the pending count, or the total metadata shape falls
    /// outside the closed wake contract.
    pub fn new(
        profile: String,
        squad: String,
        pending_count: u64,
        highest_priority: MessagePriorityDto,
        oldest_message_id: String,
    ) -> Result<Self, ActivationContractError> {
        validate_identifier(
            &profile,
            MAX_WAKE_PROFILE_BYTES,
            ActivationContractError::EmptyProfile,
            ActivationContractError::InvalidProfile,
        )?;
        validate_identifier(
            &squad,
            MAX_WAKE_SQUAD_BYTES,
            ActivationContractError::EmptySquad,
            ActivationContractError::InvalidSquad,
        )?;
        if pending_count == 0 || pending_count > MAX_WAKE_PENDING_COUNT {
            return Err(ActivationContractError::PendingCountOutOfRange);
        }
        validate_identifier(
            &oldest_message_id,
            MAX_WAKE_MESSAGE_ID_BYTES,
            ActivationContractError::EmptyOldestMessageId,
            ActivationContractError::InvalidOldestMessageId,
        )?;
        Ok(Self {
            profile,
            squad,
            pending_count,
            highest_priority,
            oldest_message_id,
        })
    }

    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    #[must_use]
    pub fn squad(&self) -> &str {
        &self.squad
    }

    #[must_use]
    pub const fn pending_count(&self) -> u64 {
        self.pending_count
    }

    #[must_use]
    pub const fn highest_priority(&self) -> MessagePriorityDto {
        self.highest_priority
    }

    #[must_use]
    pub fn oldest_message_id(&self) -> &str {
        &self.oldest_message_id
    }

    #[must_use]
    pub fn fixed_notice(&self) -> String {
        format!(
            "Psst has durable pending mail. Use the configured Psst tools to inspect the inbox; retrieval does not acknowledge. profile={} squad={} pending_count={} highest_priority={} oldest_message_id={}",
            self.profile,
            self.squad,
            self.pending_count,
            match self.highest_priority {
                MessagePriorityDto::Normal => "normal",
                MessagePriorityDto::High => "high",
            },
            self.oldest_message_id,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivationPolicy {
    pub reconcile_interval: Duration,
    pub initial_backoff: Duration,
    pub maximum_backoff: Duration,
    pub maximum_attempts: u8,
}

impl ActivationPolicy {
    /// Validates reconciliation and retry bounds.
    ///
    /// # Errors
    /// Returns an error when reconciliation exceeds one minute or retry bounds are empty,
    /// inverted, or unbounded.
    pub fn validate(self) -> Result<Self, ActivationContractError> {
        if !(MIN_RECONCILE_INTERVAL..=MAX_RECONCILE_INTERVAL).contains(&self.reconcile_interval) {
            return Err(ActivationContractError::ReconcileIntervalOutOfRange);
        }
        if self.initial_backoff.is_zero()
            || self.initial_backoff > self.maximum_backoff
            || self.maximum_backoff > MAX_BACKOFF_DURATION
            || self.maximum_attempts == 0
            || self.maximum_attempts > MAX_BACKOFF_ATTEMPTS
        {
            return Err(ActivationContractError::BackoffRangeInvalid);
        }
        Ok(self)
    }
}

impl Default for ActivationPolicy {
    fn default() -> Self {
        Self {
            reconcile_interval: DEFAULT_RECONCILE_INTERVAL,
            initial_backoff: DEFAULT_BACKOFF_INITIAL,
            maximum_backoff: DEFAULT_BACKOFF_MAX,
            maximum_attempts: MAX_BACKOFF_ATTEMPTS,
        }
    }
}

fn validate_identifier(
    value: &str,
    maximum_bytes: usize,
    empty: ActivationContractError,
    invalid: ActivationContractError,
) -> Result<(), ActivationContractError> {
    if value.is_empty() {
        return Err(empty);
    }
    if value.len() > maximum_bytes
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    fn wake(count: u64) -> WakeMetadata {
        WakeMetadata::new(
            "codex-main".to_owned(),
            "build-squad".to_owned(),
            count,
            MessagePriorityDto::Normal,
            "msg_1".to_owned(),
        )
        .unwrap()
    }

    #[test]
    fn wake_notice_is_fixed_bounded_metadata_without_participant_body() {
        let metadata = WakeMetadata::new(
            "codex-main".to_owned(),
            "build-squad".to_owned(),
            7,
            MessagePriorityDto::High,
            "msg_012345".to_owned(),
        )
        .unwrap();

        assert_eq!(
            metadata.fixed_notice(),
            "Psst has durable pending mail. Use the configured Psst tools to inspect the inbox; retrieval does not acknowledge. profile=codex-main squad=build-squad pending_count=7 highest_priority=high oldest_message_id=msg_012345"
        );
        assert!(!metadata.fixed_notice().contains("body"));
    }

    #[test]
    fn wake_metadata_rejects_markup_controls_empty_mail_and_unbounded_values() {
        for invalid in ["<channel>", "line\nbreak", "space name", "sender/name"] {
            assert_eq!(
                WakeMetadata::new(
                    invalid.to_owned(),
                    "squad".to_owned(),
                    1,
                    MessagePriorityDto::Normal,
                    "msg_1".to_owned(),
                ),
                Err(ActivationContractError::InvalidProfile)
            );
        }
        assert_eq!(
            WakeMetadata::new(
                "profile".to_owned(),
                "squad".to_owned(),
                0,
                MessagePriorityDto::Normal,
                "msg_1".to_owned(),
            ),
            Err(ActivationContractError::PendingCountOutOfRange)
        );
        assert_eq!(
            WakeMetadata::new(
                "profile".to_owned(),
                "squad".to_owned(),
                1,
                MessagePriorityDto::Normal,
                "x".repeat(MAX_WAKE_MESSAGE_ID_BYTES + 1),
            ),
            Err(ActivationContractError::InvalidOldestMessageId)
        );
    }

    #[test]
    fn activation_policy_is_bounded_and_reconciles_within_one_minute() {
        assert_eq!(
            ActivationPolicy::default().validate(),
            Ok(ActivationPolicy::default())
        );
        assert_eq!(
            ActivationPolicy {
                reconcile_interval: Duration::from_secs(61),
                ..ActivationPolicy::default()
            }
            .validate(),
            Err(ActivationContractError::ReconcileIntervalOutOfRange)
        );
        assert_eq!(
            ActivationPolicy {
                maximum_attempts: MAX_BACKOFF_ATTEMPTS + 1,
                ..ActivationPolicy::default()
            }
            .validate(),
            Err(ActivationContractError::BackoffRangeInvalid)
        );
    }

    #[test]
    fn burst_coalesces_and_never_preempts_a_running_turn() {
        let mut machine = ActivationMachine::new(ActivationPolicy::default()).unwrap();
        assert_eq!(machine.observe(None), ActivationCommand::None);
        assert_eq!(
            machine.observe(Some(wake(1))),
            ActivationCommand::Activate(wake(1))
        );
        machine.wake_started().unwrap();
        machine.wake_accepted().unwrap();

        assert_eq!(machine.observe(Some(wake(2))), ActivationCommand::None);
        assert_eq!(machine.observe(Some(wake(3))), ActivationCommand::None);
        assert_eq!(
            machine.snapshot(),
            ActivationSnapshot {
                phase: ActivationPhase::Running,
                pending: Some(wake(3)),
                reconcile_needed: true,
                retry_attempt: 0,
            }
        );
        assert_eq!(
            machine.wake_completed().unwrap(),
            ActivationCommand::ReconcileNow
        );
        assert_eq!(machine.snapshot().phase, ActivationPhase::Quiet);
    }

    #[test]
    fn retry_is_exponential_capped_and_eventually_blocks() {
        let policy = ActivationPolicy {
            initial_backoff: Duration::from_secs(2),
            maximum_backoff: Duration::from_secs(5),
            maximum_attempts: 4,
            ..ActivationPolicy::default()
        };
        let mut machine = ActivationMachine::new(policy).unwrap();
        assert!(matches!(
            machine.observe(Some(wake(1))),
            ActivationCommand::Activate(_)
        ));

        for expected in [
            ActivationCommand::RetryAfter(Duration::from_secs(2)),
            ActivationCommand::RetryAfter(Duration::from_secs(4)),
            ActivationCommand::RetryAfter(Duration::from_secs(5)),
        ] {
            machine.wake_started().unwrap();
            assert_eq!(machine.wake_retryable_failure().unwrap(), expected);
            assert!(matches!(
                machine.retry_due().unwrap(),
                ActivationCommand::Activate(_)
            ));
        }
        machine.wake_started().unwrap();
        assert_eq!(
            machine.wake_retryable_failure().unwrap(),
            ActivationCommand::None
        );
        assert_eq!(machine.snapshot().phase, ActivationPhase::Blocked);
    }

    #[test]
    fn retry_jitter_is_deterministically_bounded_to_half_through_full_delay() {
        let delay = Duration::from_millis(100);
        assert_eq!(jitter_delay_from(delay, 0), Duration::from_millis(50));
        assert_eq!(jitter_delay_from(delay, 50), Duration::from_millis(100));
        assert_eq!(jitter_delay_from(delay, 51), Duration::from_millis(50));
        assert_eq!(
            jitter_delay_from(Duration::from_millis(1), u64::MAX),
            Duration::from_millis(1)
        );
    }

    #[test]
    fn invalid_transitions_fail_closed_and_stop_is_terminal() {
        let mut machine = ActivationMachine::new(ActivationPolicy::default()).unwrap();
        assert_eq!(
            machine.wake_accepted(),
            Err(ActivationContractError::InvalidTransition)
        );
        assert_eq!(
            machine.retry_due(),
            Err(ActivationContractError::InvalidTransition)
        );
        machine.observe(Some(wake(1)));
        machine.stop();
        assert_eq!(machine.observe(Some(wake(2))), ActivationCommand::None);
        assert_eq!(machine.snapshot().phase, ActivationPhase::Stopped);
        assert!(machine.snapshot().pending.is_none());
    }

    struct FakeSource {
        values: Mutex<VecDeque<Result<Option<WakeMetadata>, ObservationFailure>>>,
    }

    struct RecordingSource {
        values: Mutex<VecDeque<Result<Option<WakeMetadata>, ObservationFailure>>>,
        waits: Mutex<Vec<Duration>>,
    }

    struct PendingSource;

    impl ActivationSource for PendingSource {
        fn observe(
            &self,
            _maximum_wait: Duration,
        ) -> ActivationFuture<'_, Result<Option<WakeMetadata>, ObservationFailure>> {
            Box::pin(std::future::pending())
        }
    }

    impl ActivationSource for RecordingSource {
        fn observe(
            &self,
            maximum_wait: Duration,
        ) -> ActivationFuture<'_, Result<Option<WakeMetadata>, ObservationFailure>> {
            Box::pin(async move {
                self.waits.lock().unwrap().push(maximum_wait);
                if let Some(value) = self.values.lock().unwrap().pop_front() {
                    value
                } else {
                    tokio::time::sleep(maximum_wait).await;
                    Ok(None)
                }
            })
        }
    }

    impl ActivationSource for FakeSource {
        fn observe(
            &self,
            maximum_wait: Duration,
        ) -> ActivationFuture<'_, Result<Option<WakeMetadata>, ObservationFailure>> {
            Box::pin(async move {
                if let Some(value) = self.values.lock().unwrap().pop_front() {
                    value
                } else {
                    tokio::time::sleep(maximum_wait).await;
                    Ok(None)
                }
            })
        }
    }

    struct ImmediateTurn(Result<(), HostFailure>);

    impl ActivationTurn for ImmediateTurn {
        fn completed(self: Box<Self>) -> ActivationFuture<'static, Result<(), HostFailure>> {
            Box::pin(async move { self.0 })
        }
    }

    struct FakeHost {
        starts: AtomicUsize,
        completions: Mutex<VecDeque<Result<(), HostFailure>>>,
    }

    impl ActivationHost for FakeHost {
        fn start<'a>(
            &'a self,
            _wake: &'a WakeMetadata,
        ) -> ActivationFuture<'a, Result<Box<dyn ActivationTurn>, HostFailure>> {
            Box::pin(async move {
                self.starts.fetch_add(1, Ordering::SeqCst);
                let completion = self
                    .completions
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Ok(()));
                Ok(Box::new(ImmediateTurn(completion)) as Box<dyn ActivationTurn>)
            })
        }
    }

    #[tokio::test]
    async fn owned_runtime_recovers_an_initial_empty_poll_without_empty_activation() {
        let source = Arc::new(FakeSource {
            values: Mutex::new(VecDeque::from([Ok(None), Ok(Some(wake(4)))])),
        });
        let host = Arc::new(FakeHost {
            starts: AtomicUsize::new(0),
            completions: Mutex::new(VecDeque::new()),
        });
        let runtime =
            ActivationRuntime::start(source, host.clone(), ActivationPolicy::default()).unwrap();
        for _ in 0..100 {
            if host.starts.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(host.starts.load(Ordering::SeqCst), 1);
        runtime.shutdown().await;
        assert_eq!(runtime.snapshot().await.phase, ActivationPhase::Stopped);
    }

    #[tokio::test]
    async fn completed_turn_is_followed_by_immediate_mailbox_reconciliation() {
        let source = Arc::new(RecordingSource {
            values: Mutex::new(VecDeque::from([Ok(Some(wake(1))), Ok(None)])),
            waits: Mutex::new(Vec::new()),
        });
        let host = Arc::new(FakeHost {
            starts: AtomicUsize::new(0),
            completions: Mutex::new(VecDeque::new()),
        });
        let runtime = ActivationRuntime::start(
            source.clone(),
            host,
            ActivationPolicy {
                reconcile_interval: Duration::from_secs(60),
                ..ActivationPolicy::default()
            },
        )
        .unwrap();
        for _ in 0..100 {
            if source.waits.lock().unwrap().len() >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        let waits = source.waits.lock().unwrap().clone();
        assert!(waits.len() >= 2);
        assert_eq!(waits[0], Duration::from_secs(30));
        assert_eq!(waits[1], Duration::ZERO);
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_cancels_a_dropped_long_poll_and_restart_recovers_relay_truth() {
        let quiet_host = Arc::new(FakeHost {
            starts: AtomicUsize::new(0),
            completions: Mutex::new(VecDeque::new()),
        });
        let runtime = ActivationRuntime::start(
            Arc::new(PendingSource),
            quiet_host.clone(),
            ActivationPolicy::default(),
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), runtime.shutdown())
            .await
            .expect("shutdown must cancel an outstanding observation");
        assert_eq!(quiet_host.starts.load(Ordering::SeqCst), 0);

        let recovered_host = Arc::new(FakeHost {
            starts: AtomicUsize::new(0),
            completions: Mutex::new(VecDeque::new()),
        });
        let restarted = ActivationRuntime::start(
            Arc::new(FakeSource {
                values: Mutex::new(VecDeque::from([Ok(Some(wake(7)))])),
            }),
            recovered_host.clone(),
            ActivationPolicy::default(),
        )
        .unwrap();
        for _ in 0..100 {
            if recovered_host.starts.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(recovered_host.starts.load(Ordering::SeqCst), 1);
        restarted.shutdown().await;
    }

    #[tokio::test]
    async fn ambiguous_accepted_turn_completion_blocks_without_duplicate_start() {
        let source = Arc::new(FakeSource {
            values: Mutex::new(VecDeque::from([Ok(Some(wake(1)))])),
        });
        let host = Arc::new(FakeHost {
            starts: AtomicUsize::new(0),
            completions: Mutex::new(VecDeque::from([Err(HostFailure::OutcomeUnknown)])),
        });
        let runtime =
            ActivationRuntime::start(source, host.clone(), ActivationPolicy::default()).unwrap();
        for _ in 0..100 {
            if runtime.snapshot().await.phase == ActivationPhase::Blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(runtime.snapshot().await.phase, ActivationPhase::Blocked);
        assert_eq!(host.starts.load(Ordering::SeqCst), 1);
        runtime.shutdown().await;
    }
}
