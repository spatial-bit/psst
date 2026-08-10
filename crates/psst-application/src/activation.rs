use psst_protocol::MessagePriorityDto;
use std::{fmt, time::Duration};

pub const MAX_WAKE_PENDING_COUNT: u64 = 1_000_000;
pub const MAX_WAKE_PROFILE_BYTES: usize = 64;
pub const MAX_WAKE_SQUAD_BYTES: usize = 64;
pub const MAX_WAKE_MESSAGE_ID_BYTES: usize = 128;
pub const DEFAULT_RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
pub const MIN_RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
pub const MAX_RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
pub const DEFAULT_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
pub const DEFAULT_BACKOFF_MAX: Duration = Duration::from_secs(60);
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
        })
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
}
