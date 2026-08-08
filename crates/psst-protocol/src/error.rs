use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ErrorBody {
    pub code: ApiErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(default)]
    pub details: BTreeMap<String, String>,
}

impl ApiErrorCode {
    #[must_use]
    pub const fn http_status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::NotFound | Self::RecipientNotFound => 404,
            Self::NotMember => 403,
            Self::PayloadTooLarge => 413,
            Self::SquadArchived
            | Self::NameInUse
            | Self::LeaseExpired
            | Self::IdempotencyConflict => 409,
            Self::RateLimited => 429,
            Self::DatabaseBusy => 503,
            Self::InternalError => 500,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_envelope_is_stable_and_unknown_codes_fail() {
        let json = r#"{"error":{"code":"name_in_use","message":"The name is in use.","retryable":false,"details":{}}}"#;
        let value: ErrorEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(value.error.code, ApiErrorCode::NameInUse);
        assert_eq!(serde_json::to_string(&value).unwrap(), json);
        assert!(
            serde_json::from_str::<ErrorEnvelope>(&json.replace("name_in_use", "future_code"))
                .is_err()
        );
    }

    #[test]
    fn every_error_code_spelling_and_status_is_stable() {
        let cases = [
            ("invalid_request", 400),
            ("not_found", 404),
            ("squad_archived", 409),
            ("not_member", 403),
            ("name_in_use", 409),
            ("lease_expired", 409),
            ("recipient_not_found", 404),
            ("idempotency_conflict", 409),
            ("payload_too_large", 413),
            ("rate_limited", 429),
            ("database_busy", 503),
            ("internal_error", 500),
        ];
        for (spelling, status) in cases {
            let code: ApiErrorCode = serde_json::from_str(&format!("\"{spelling}\"")).unwrap();
            assert_eq!(code.http_status(), status);
            assert_eq!(
                serde_json::to_string(&code).unwrap(),
                format!("\"{spelling}\"")
            );
        }
    }
}
