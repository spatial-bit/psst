use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorCode {
    InvalidRequest,
    NotFound,
    SquadArchived,
    NotMember,
    NameInUse,
    LeaseExpired,
    RecipientNotFound,
    IdempotencyConflict,
    PayloadTooLarge,
    RateLimited,
    DatabaseBusy,
    InternalError,
    InvalidStateTransition,
}

impl ErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::NotFound => "not_found",
            Self::SquadArchived => "squad_archived",
            Self::NotMember => "not_member",
            Self::NameInUse => "name_in_use",
            Self::LeaseExpired => "lease_expired",
            Self::RecipientNotFound => "recipient_not_found",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::PayloadTooLarge => "payload_too_large",
            Self::RateLimited => "rate_limited",
            Self::DatabaseBusy => "database_busy",
            Self::InternalError => "internal_error",
            Self::InvalidStateTransition => "invalid_state_transition",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidValue {
    field: &'static str,
    reason: &'static str,
}

impl InvalidValue {
    #[must_use]
    pub const fn new(field: &'static str, reason: &'static str) -> Self {
        Self { field, reason }
    }

    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }

    #[must_use]
    pub const fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for InvalidValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}: {}", self.field, self.reason)
    }
}

impl Error for InvalidValue {}

#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DomainError {
    InvalidValue(InvalidValue),
    InvalidStateTransition {
        entity: &'static str,
        action: &'static str,
    },
    LeaseExpired,
    IdempotencyConflict {
        existing: crate::MessageId,
    },
}

impl DomainError {
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::InvalidValue(_) => ErrorCode::InvalidRequest,
            Self::InvalidStateTransition { .. } => ErrorCode::InvalidStateTransition,
            Self::LeaseExpired => ErrorCode::LeaseExpired,
            Self::IdempotencyConflict { .. } => ErrorCode::IdempotencyConflict,
        }
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue(error) => error.fmt(f),
            Self::InvalidStateTransition { entity, action } => {
                write!(f, "cannot {action} {entity} in its current state")
            }
            Self::LeaseExpired => f.write_str("the instance lease has expired"),
            Self::IdempotencyConflict { .. } => f.write_str("dedupe key has different semantics"),
        }
    }
}

impl Error for DomainError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_protocol_error_codes_are_stable() {
        let cases = [
            (ErrorCode::InvalidRequest, "invalid_request"),
            (ErrorCode::NotFound, "not_found"),
            (ErrorCode::SquadArchived, "squad_archived"),
            (ErrorCode::NotMember, "not_member"),
            (ErrorCode::NameInUse, "name_in_use"),
            (ErrorCode::LeaseExpired, "lease_expired"),
            (ErrorCode::RecipientNotFound, "recipient_not_found"),
            (ErrorCode::IdempotencyConflict, "idempotency_conflict"),
            (ErrorCode::PayloadTooLarge, "payload_too_large"),
            (ErrorCode::RateLimited, "rate_limited"),
            (ErrorCode::DatabaseBusy, "database_busy"),
            (ErrorCode::InternalError, "internal_error"),
            (
                ErrorCode::InvalidStateTransition,
                "invalid_state_transition",
            ),
        ];
        for (code, expected) in cases {
            assert_eq!(code.as_str(), expected);
        }
    }
}
