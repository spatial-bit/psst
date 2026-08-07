use crate::{
    AckMessagesRequest, AvailabilityDto, AvailabilitySourceDto, ClientMetadata, CreateSquadRequest,
    ErrorBody, HeartbeatRequest, InboxQuery, InboxResponse, JoinSquadRequest, ResumeSquadRequest,
    SendMessageRequest, TranscriptQuery,
};
use psst_core::{
    CorrelationId, DedupeKey, InvalidValue, MemberName, MessageBody, MessageId, Mission, Role,
    SquadName,
};

pub const MAX_INBOX_MESSAGES: u16 = 100;
pub const MAX_INBOX_BYTES: usize = 1024 * 1024;
pub const MAX_WAIT_SECONDS: u8 = 30;
pub const MAX_ACK_MESSAGES: usize = 100;
pub const MAX_ERROR_MESSAGE_BYTES: usize = 512;
pub const MAX_ERROR_DETAILS: usize = 16;
pub const MAX_ERROR_DETAIL_KEY_BYTES: usize = 64;
pub const MAX_ERROR_DETAIL_VALUE_BYTES: usize = 256;

pub trait Validate {
    /// Validates all protocol bounds before store dispatch.
    ///
    /// # Errors
    /// Returns the first stable invalid-value result.
    fn validate(&self) -> Result<(), InvalidValue>;
}

impl Validate for InboxQuery {
    fn validate(&self) -> Result<(), InvalidValue> {
        if self.limit == 0 || self.limit > MAX_INBOX_MESSAGES {
            return Err(InvalidValue::new("limit", "must be between 1 and 100"));
        }
        if self.wait_seconds > MAX_WAIT_SECONDS {
            return Err(InvalidValue::new(
                "wait",
                "must be between 0 and 30 seconds",
            ));
        }
        Ok(())
    }
}

impl Validate for TranscriptQuery {
    fn validate(&self) -> Result<(), InvalidValue> {
        if self.limit == 0 || self.limit > MAX_INBOX_MESSAGES {
            return Err(InvalidValue::new("limit", "must be between 1 and 100"));
        }
        Ok(())
    }
}

impl Validate for CreateSquadRequest {
    fn validate(&self) -> Result<(), InvalidValue> {
        SquadName::new(&self.name)?;
        Mission::new(&self.mission)?;
        Ok(())
    }
}

fn validate_client(client: &ClientMetadata) -> Result<(), InvalidValue> {
    if client.kind.is_empty() || client.kind.len() > 64 || client.kind.chars().any(char::is_control)
    {
        return Err(InvalidValue::new(
            "client.kind",
            "must be printable and at most 64 bytes",
        ));
    }
    if client
        .hostname
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > 255)
    {
        return Err(InvalidValue::new(
            "client.hostname",
            "must be between 1 and 255 bytes",
        ));
    }
    if client
        .version
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > 64)
    {
        return Err(InvalidValue::new(
            "client.version",
            "must be between 1 and 64 bytes",
        ));
    }
    Ok(())
}

impl Validate for JoinSquadRequest {
    fn validate(&self) -> Result<(), InvalidValue> {
        MemberName::new(&self.name)?;
        Role::new(&self.role)?;
        if let Some(mission) = &self.mission {
            Mission::new(mission)?;
        }
        validate_client(&self.client)
    }
}

impl Validate for ResumeSquadRequest {
    fn validate(&self) -> Result<(), InvalidValue> {
        validate_client(&self.client)
    }
}

impl Validate for HeartbeatRequest {
    fn validate(&self) -> Result<(), InvalidValue> {
        let availability_unknown = self.availability == AvailabilityDto::Unknown;
        let source_unknown = self.availability_source == AvailabilitySourceDto::Unknown;
        if availability_unknown != source_unknown {
            return Err(InvalidValue::new(
                "availability",
                "availability and source must both be known or unknown",
            ));
        }
        Ok(())
    }
}

impl Validate for SendMessageRequest {
    fn validate(&self) -> Result<(), InvalidValue> {
        MemberName::new(&self.recipient)?;
        MessageBody::new(&self.body)?;
        DedupeKey::new(&self.dedupe_key)?;
        if let Some(value) = &self.reply_to {
            MessageId::new(value)?;
        }
        if let Some(value) = &self.correlation_id {
            CorrelationId::new(value)?;
        }
        Ok(())
    }
}

impl Validate for AckMessagesRequest {
    fn validate(&self) -> Result<(), InvalidValue> {
        if self.message_ids.is_empty() || self.message_ids.len() > MAX_ACK_MESSAGES {
            return Err(InvalidValue::new(
                "message_ids",
                "must contain between 1 and 100 IDs",
            ));
        }
        for id in &self.message_ids {
            MessageId::new(id)?;
        }
        Ok(())
    }
}

impl Validate for InboxResponse {
    fn validate(&self) -> Result<(), InvalidValue> {
        if self.messages.len() > usize::from(MAX_INBOX_MESSAGES) {
            return Err(InvalidValue::new(
                "messages",
                "exceeds the 100-message response bound",
            ));
        }
        Ok(())
    }
}

/// Serializes an inbox and rejects the actual encoded JSON when it exceeds 1 MiB.
///
/// # Errors
/// Returns an invalid-value error when serialization fails or the encoded body exceeds 1 MiB.
pub fn encode_bounded_inbox(response: &InboxResponse) -> Result<Vec<u8>, InvalidValue> {
    let encoded = serde_json::to_vec(response)
        .map_err(|_| InvalidValue::new("inbox", "could not serialize response"))?;
    if encoded.len() > MAX_INBOX_BYTES {
        return Err(InvalidValue::new(
            "inbox",
            "serialized response exceeds 1 MiB",
        ));
    }
    Ok(encoded)
}

impl Validate for ErrorBody {
    fn validate(&self) -> Result<(), InvalidValue> {
        if self.message.is_empty() || self.message.len() > MAX_ERROR_MESSAGE_BYTES {
            return Err(InvalidValue::new(
                "error.message",
                "must be between 1 and 512 bytes",
            ));
        }
        if self.details.len() > MAX_ERROR_DETAILS {
            return Err(InvalidValue::new(
                "error.details",
                "must contain at most 16 entries",
            ));
        }
        if self.details.iter().any(|(key, value)| {
            key.is_empty()
                || key.len() > MAX_ERROR_DETAIL_KEY_BYTES
                || value.len() > MAX_ERROR_DETAIL_VALUE_BYTES
        }) {
            return Err(InvalidValue::new(
                "error.details",
                "contains an invalid key or value bound",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApiTimestamp, MessageDto, MessagePriorityDto, MessageSequence};

    #[test]
    fn protocol_bounds_are_enforced() {
        assert!(
            InboxQuery {
                limit: 100,
                wait_seconds: 30
            }
            .validate()
            .is_ok()
        );
        assert!(
            InboxQuery {
                limit: 101,
                wait_seconds: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            TranscriptQuery {
                after: crate::MessageSequence::new(i64::MAX).unwrap(),
                limit: 100
            }
            .validate()
            .is_ok()
        );
        assert!(
            AckMessagesRequest {
                message_ids: Vec::new()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_request_family_rejects_invalid_values_at_validate_boundary() {
        let client = ClientMetadata {
            kind: "codex".into(),
            hostname: None,
            version: None,
        };
        for request in [
            CreateSquadRequest {
                name: String::new(),
                mission: "m".into(),
            },
            CreateSquadRequest {
                name: "Bad".into(),
                mission: "m".into(),
            },
            CreateSquadRequest {
                name: "good".into(),
                mission: " whitespace ".into(),
            },
            CreateSquadRequest {
                name: "good".into(),
                mission: "x".repeat(4097),
            },
        ] {
            assert!(request.validate().is_err());
        }
        for request in [
            JoinSquadRequest {
                name: " bad".into(),
                role: "r".into(),
                mode: crate::AgentModeDto::Cooperative,
                client: client.clone(),
                mission: None,
            },
            JoinSquadRequest {
                name: "good".into(),
                role: " ".into(),
                mode: crate::AgentModeDto::Cooperative,
                client: client.clone(),
                mission: None,
            },
            JoinSquadRequest {
                name: "good".into(),
                role: "r".into(),
                mode: crate::AgentModeDto::Cooperative,
                client: client.clone(),
                mission: Some(" ".into()),
            },
        ] {
            assert!(request.validate().is_err());
        }
        assert!(
            ResumeSquadRequest {
                mode: crate::AgentModeDto::Scheduled,
                client: ClientMetadata {
                    kind: String::new(),
                    hostname: None,
                    version: None
                }
            }
            .validate()
            .is_err()
        );
        let observed: ApiTimestamp = serde_json::from_str("\"2026-08-07T01:02:03.004Z\"").unwrap();
        assert!(
            HeartbeatRequest {
                availability: AvailabilityDto::Idle,
                availability_source: AvailabilitySourceDto::Unknown,
                availability_observed_at: observed
            }
            .validate()
            .is_err()
        );
        let base = SendMessageRequest {
            recipient: "recipient".into(),
            body: "body".into(),
            priority: MessagePriorityDto::Normal,
            dedupe_key: "dedupe".into(),
            reply_to: None,
            correlation_id: None,
        };
        let mut invalid = Vec::new();
        let mut value = base.clone();
        value.recipient = "Bad".into();
        invalid.push(value);
        let mut value = base.clone();
        value.body = String::new();
        invalid.push(value);
        let mut value = base.clone();
        value.body = "x".repeat(65_537);
        invalid.push(value);
        let mut value = base.clone();
        value.dedupe_key = " whitespace ".into();
        invalid.push(value);
        let mut value = base.clone();
        value.reply_to = Some("bad".into());
        invalid.push(value);
        let mut value = base;
        value.correlation_id = Some("line\nbreak".into());
        invalid.push(value);
        assert!(invalid.iter().all(|request| request.validate().is_err()));
        assert!(
            InboxQuery {
                limit: 0,
                wait_seconds: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            InboxQuery {
                limit: 1,
                wait_seconds: 31
            }
            .validate()
            .is_err()
        );
        assert!(
            TranscriptQuery {
                after: MessageSequence::default(),
                limit: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            TranscriptQuery {
                after: MessageSequence::default(),
                limit: 101
            }
            .validate()
            .is_err()
        );
        assert!(
            AckMessagesRequest {
                message_ids: vec!["bad".into()]
            }
            .validate()
            .is_err()
        );
        assert!(
            AckMessagesRequest {
                message_ids: vec!["msg_one".into(); 101]
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn actual_json_bytes_not_reported_metadata_enforce_one_mibibyte() {
        let timestamp: ApiTimestamp = serde_json::from_str("\"2026-08-07T01:02:03.004Z\"").unwrap();
        let message = MessageDto {
            sequence: MessageSequence::new(1).unwrap(),
            id: "msg_one".into(),
            squad: "alpha".into(),
            sender: "sender".into(),
            recipient: "recipient".into(),
            body: "x".repeat(64 * 1024),
            priority: MessagePriorityDto::Normal,
            reply_to: None,
            correlation_id: None,
            created_at: timestamp,
            acknowledged_at: None,
        };
        let dishonest = InboxResponse {
            messages: vec![message; 17],
            pending_count: 17,
        };
        assert!(
            dishonest.validate().is_ok(),
            "count alone cannot detect encoded size"
        );
        assert!(encode_bounded_inbox(&dishonest).is_err());
        let honest = InboxResponse {
            messages: Vec::new(),
            pending_count: 0,
        };
        assert!(encode_bounded_inbox(&honest).is_ok());
    }
}
