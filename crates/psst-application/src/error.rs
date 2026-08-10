use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ExitClass {
    Success = 0,
    Usage = 2,
    Configuration = 3,
    Unavailable = 4,
    Conflict = 5,
    Authority = 6,
    OutcomeUnknown = 7,
    LocalIo = 8,
    Locked = 9,
    Internal = 70,
}
impl ExitClass {
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }
}

/// Closed application vocabulary. Relay protocol codes retain their semantic identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalErrorCode {
    InvalidRequest,
    NotFound,
    NotMember,
    NameInUse,
    LeaseExpired,
    RecipientNotFound,
    IdempotencyConflict,
    RateLimited,
    DatabaseBusy,
    InternalError,
    InvalidInput,
    InvalidConfiguration,
    InvalidOrigin,
    ProfileNotFound,
    ProfileLocked,
    ProfileOriginMismatch,
    ProfileAlreadyBound,
    ProfileUnbound,
    ConfigRead,
    ConfigWrite,
    LocalRead,
    LocalWrite,
    LocalPermission,
    LocalLock,
    RelayUnavailable,
    RelayTimeout,
    RelayProtocol,
    InvalidSession,
    InactiveMembership,
    UnknownRecipient,
    DuplicateName,
    SquadNotFound,
    SquadArchived,
    MessageNotFound,
    Conflict,
    AuthorityDenied,
    PayloadTooLarge,
    OutcomeUnknown,
    Unsupported,
    Internal,
}

impl LocalErrorCode {
    #[must_use]
    pub const fn exit_class(self) -> ExitClass {
        match self {
            Self::InvalidInput
            | Self::PayloadTooLarge
            | Self::Unsupported
            | Self::InvalidRequest => ExitClass::Usage,
            Self::InvalidConfiguration
            | Self::InvalidOrigin
            | Self::ProfileOriginMismatch
            | Self::ConfigRead => ExitClass::Configuration,
            Self::RelayUnavailable
            | Self::RelayTimeout
            | Self::ProfileNotFound
            | Self::ProfileUnbound
            | Self::SquadNotFound
            | Self::MessageNotFound
            | Self::NotFound
            | Self::RateLimited
            | Self::DatabaseBusy => ExitClass::Unavailable,
            Self::Conflict
            | Self::ProfileAlreadyBound
            | Self::DuplicateName
            | Self::SquadArchived
            | Self::InactiveMembership
            | Self::NameInUse
            | Self::LeaseExpired
            | Self::IdempotencyConflict => ExitClass::Conflict,
            Self::AuthorityDenied
            | Self::InvalidSession
            | Self::UnknownRecipient
            | Self::NotMember
            | Self::RecipientNotFound => ExitClass::Authority,
            Self::OutcomeUnknown => ExitClass::OutcomeUnknown,
            Self::ConfigWrite | Self::LocalRead | Self::LocalWrite | Self::LocalPermission => {
                ExitClass::LocalIo
            }
            Self::ProfileLocked | Self::LocalLock => ExitClass::Locked,
            Self::RelayProtocol | Self::Internal | Self::InternalError => ExitClass::Internal,
        }
    }
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::ProfileLocked
                | Self::LocalLock
                | Self::RelayUnavailable
                | Self::RelayTimeout
                | Self::OutcomeUnknown
                | Self::RateLimited
                | Self::DatabaseBusy
        )
    }
    #[must_use]
    pub const fn safe_message(self) -> &'static str {
        match self {
            Self::InvalidRequest => "The relay rejected the request.",
            Self::NotFound => "The requested relay resource does not exist.",
            Self::NotMember => "The session is not an active member.",
            Self::NameInUse => "The member name is already in use.",
            Self::LeaseExpired => "The relay lease expired.",
            Self::RecipientNotFound => "The relay recipient does not exist.",
            Self::IdempotencyConflict => "The retry identity conflicts with a prior request.",
            Self::RateLimited => "The relay is temporarily rate limited.",
            Self::DatabaseBusy => "The relay database is temporarily busy.",
            Self::InternalError => "The relay reported an internal error.",
            Self::InvalidInput => "The request is invalid.",
            Self::InvalidConfiguration => "The configuration is invalid.",
            Self::InvalidOrigin => "The relay origin is invalid.",
            Self::ProfileNotFound => "The selected profile does not exist.",
            Self::ProfileLocked => "The selected profile is already in use.",
            Self::ProfileOriginMismatch => "The relay origin does not match the selected profile.",
            Self::ProfileAlreadyBound => "The selected profile is already bound.",
            Self::ProfileUnbound => "The selected profile is not bound.",
            Self::ConfigRead => "The configuration could not be read.",
            Self::ConfigWrite => "The configuration could not be written.",
            Self::LocalRead => "Local data could not be read.",
            Self::LocalWrite => "Local data could not be written.",
            Self::LocalPermission => "Local data access was denied.",
            Self::LocalLock => "A required local lock is unavailable.",
            Self::RelayUnavailable => "The relay is unavailable.",
            Self::RelayTimeout => "The relay request timed out.",
            Self::RelayProtocol => "The relay response was invalid.",
            Self::InvalidSession => "The session is invalid.",
            Self::InactiveMembership => "The membership is inactive.",
            Self::UnknownRecipient => "The recipient is unknown.",
            Self::DuplicateName => "The selected local name is already in use.",
            Self::SquadNotFound => "The squad does not exist.",
            Self::SquadArchived => "The squad is archived.",
            Self::MessageNotFound => "The message does not exist.",
            Self::Conflict => "The operation conflicts with current state.",
            Self::AuthorityDenied => "The operation is not authorized.",
            Self::PayloadTooLarge => "The payload exceeds a configured limit.",
            Self::OutcomeUnknown => "The operation outcome is unknown.",
            Self::Unsupported => "The operation is not supported.",
            Self::Internal => "An internal error occurred.",
        }
    }

    /// True only when the caller must resume before attempting another protected operation.
    #[must_use]
    pub const fn requires_resume(self) -> bool {
        matches!(self, Self::LeaseExpired)
    }
}
impl fmt::Display for LocalErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_message())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafeError {
    code: LocalErrorCode,
    message: String,
    retryable: bool,
    exit_class: ExitClass,
}
impl SafeError {
    #[must_use]
    pub const fn code(&self) -> LocalErrorCode {
        self.code
    }
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
    #[must_use]
    pub const fn retryable(&self) -> bool {
        self.retryable
    }
    #[must_use]
    pub const fn exit_class(&self) -> ExitClass {
        self.exit_class
    }
}
impl From<LocalErrorCode> for SafeError {
    fn from(code: LocalErrorCode) -> Self {
        Self {
            code,
            message: code.safe_message().into(),
            retryable: code.retryable(),
            exit_class: code.exit_class(),
        }
    }
}

/// MCP tool failures intentionally omit process-only exit information.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpSafeError {
    pub code: LocalErrorCode,
    pub message: String,
    pub retryable: bool,
}
impl From<LocalErrorCode> for McpSafeError {
    fn from(code: LocalErrorCode) -> Self {
        Self {
            code,
            message: code.safe_message().into(),
            retryable: code.retryable(),
        }
    }
}

#[must_use]
pub const fn map_api_error(code: psst_protocol::ApiErrorCode) -> LocalErrorCode {
    use psst_protocol::ApiErrorCode as Api;
    match code {
        Api::InvalidRequest => LocalErrorCode::InvalidRequest,
        Api::NotFound => LocalErrorCode::NotFound,
        Api::SquadArchived => LocalErrorCode::SquadArchived,
        Api::NotMember => LocalErrorCode::NotMember,
        Api::NameInUse => LocalErrorCode::NameInUse,
        Api::LeaseExpired => LocalErrorCode::LeaseExpired,
        Api::RecipientNotFound => LocalErrorCode::RecipientNotFound,
        Api::IdempotencyConflict => LocalErrorCode::IdempotencyConflict,
        Api::PayloadTooLarge => LocalErrorCode::PayloadTooLarge,
        Api::RateLimited => LocalErrorCode::RateLimited,
        Api::DatabaseBusy => LocalErrorCode::DatabaseBusy,
        Api::InternalError => LocalErrorCode::InternalError,
    }
}

#[must_use]
pub fn map_client_error(error: &psst_client::Error) -> LocalErrorCode {
    match error {
        psst_client::Error::InvalidBaseUrl => LocalErrorCode::InvalidOrigin,
        psst_client::Error::InvalidConfiguration => LocalErrorCode::InvalidConfiguration,
        psst_client::Error::InvalidRequest => LocalErrorCode::InvalidInput,
        psst_client::Error::MalformedCredential | psst_client::Error::MalformedResponse { .. } => {
            LocalErrorCode::RelayProtocol
        }
        psst_client::Error::Transport(_) => LocalErrorCode::RelayUnavailable,
        psst_client::Error::Timeout => LocalErrorCode::RelayTimeout,
        psst_client::Error::OutcomeUnknown => LocalErrorCode::OutcomeUnknown,
        psst_client::Error::Api { code, .. } => map_api_error(*code),
        psst_client::Error::ResponseTooLarge => LocalErrorCode::PayloadTooLarge,
        psst_client::Error::UnexpectedHttp { .. } => LocalErrorCode::RelayProtocol,
        psst_client::Error::ClientBusy => LocalErrorCode::LocalLock,
        psst_client::Error::RetryExhausted { last, .. } => map_client_error(last),
    }
}
