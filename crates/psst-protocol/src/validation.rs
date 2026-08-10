use crate::{
    AckMessagesRequest, AckMessagesResponse, ArchiveSquadResponse, AvailabilityDto,
    AvailabilitySourceDto, ClientMetadata, CreateSquadRequest, ErrorBody, HeartbeatRequest,
    InboxQuery, InboxResponse, JoinSquadRequest, LeaveSquadResponse, MembershipStateDto,
    MessageDto, ResumeSquadRequest, RosterResponse, SendMessageRequest, SendMessageResponse,
    SquadStateDto, SquadSummary, TranscriptQuery, TranscriptResponse, TransportPresenceDto,
};
use psst_core::{
    CorrelationId, DedupeKey, InvalidValue, MemberName, MembershipId, MessageBody, MessageId,
    Mission, Role, SquadId, SquadName,
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
        validate_message_page(&self.messages)?;
        if self.pending_count < self.messages.len() as u64 {
            return Err(InvalidValue::new(
                "pending_count",
                "must include every returned message",
            ));
        }
        match self.pending_count {
            0 if self.messages.is_empty()
                && self.highest_priority.is_none()
                && self.oldest_message_id.is_none() =>
            {
                Ok(())
            }
            1.. if !self.messages.is_empty()
                && self.highest_priority.is_some()
                && self.oldest_message_id.as_deref()
                    == self.messages.first().map(|message| message.id.as_str()) =>
            {
                MessageId::new(
                    self.oldest_message_id
                        .as_deref()
                        .expect("guarded oldest id"),
                )?;
                Ok(())
            }
            _ => Err(InvalidValue::new(
                "inbox",
                "has inconsistent pending activation metadata",
            )),
        }
    }
}

impl Validate for SquadSummary {
    fn validate(&self) -> Result<(), InvalidValue> {
        SquadId::new(&self.id)?;
        SquadName::new(&self.name)?;
        Mission::new(&self.mission)?;
        let archive_consistent = match self.state {
            SquadStateDto::Active => self.archived_at.is_none(),
            SquadStateDto::Archived => self
                .archived_at
                .is_some_and(|value| value >= self.created_at),
        };
        archive_consistent
            .then_some(())
            .ok_or_else(|| InvalidValue::new("squad", "has inconsistent archive state"))
    }
}

impl Validate for Vec<SquadSummary> {
    fn validate(&self) -> Result<(), InvalidValue> {
        for squad in self {
            squad.validate()?;
        }
        Ok(())
    }
}

impl Validate for ArchiveSquadResponse {
    fn validate(&self) -> Result<(), InvalidValue> {
        self.squad.validate()
    }
}

impl Validate for LeaveSquadResponse {
    fn validate(&self) -> Result<(), InvalidValue> {
        MembershipId::new(&self.membership_id).map(|_| ())
    }
}

impl Validate for RosterResponse {
    fn validate(&self) -> Result<(), InvalidValue> {
        SquadName::new(&self.squad)?;
        for member in &self.members {
            MembershipId::new(&member.membership_id)?;
            MemberName::new(&member.name)?;
            Role::new(&member.role)?;
            let availability_unknown = member.availability == AvailabilityDto::Unknown;
            let source_unknown = member.availability_source == AvailabilitySourceDto::Unknown;
            if availability_unknown != source_unknown
                || (member.membership_state == MembershipStateDto::Left
                    && member.presence != TransportPresenceDto::Offline)
            {
                return Err(InvalidValue::new("roster", "has inconsistent member state"));
            }
        }
        Ok(())
    }
}

impl Validate for MessageDto {
    fn validate(&self) -> Result<(), InvalidValue> {
        MessageId::new(&self.id)?;
        SquadName::new(&self.squad)?;
        MemberName::new(&self.sender)?;
        MemberName::new(&self.recipient)?;
        MessageBody::new(&self.body)?;
        if let Some(value) = &self.reply_to {
            MessageId::new(value)?;
        }
        if let Some(value) = &self.correlation_id {
            CorrelationId::new(value)?;
        }
        if self
            .acknowledged_at
            .is_some_and(|value| value < self.created_at)
        {
            return Err(InvalidValue::new("message", "has inconsistent timestamps"));
        }
        Ok(())
    }
}

impl Validate for SendMessageResponse {
    fn validate(&self) -> Result<(), InvalidValue> {
        self.message.validate()
    }
}

impl Validate for AckMessagesResponse {
    fn validate(&self) -> Result<(), InvalidValue> {
        if self.acknowledged_ids.is_empty() || self.acknowledged_ids.len() > MAX_ACK_MESSAGES {
            return Err(InvalidValue::new(
                "acknowledged_ids",
                "must contain between 1 and 100 IDs",
            ));
        }
        for id in &self.acknowledged_ids {
            MessageId::new(id)?;
        }
        Ok(())
    }
}

impl Validate for TranscriptResponse {
    fn validate(&self) -> Result<(), InvalidValue> {
        if self.messages.len() > usize::from(MAX_INBOX_MESSAGES) {
            return Err(InvalidValue::new(
                "messages",
                "exceeds the 100-message response bound",
            ));
        }
        validate_message_page(&self.messages)?;
        if self.next_after != self.messages.last().map(|message| message.sequence) {
            return Err(InvalidValue::new(
                "next_after",
                "does not match the final message",
            ));
        }
        Ok(())
    }
}

fn validate_message_page(messages: &[MessageDto]) -> Result<(), InvalidValue> {
    let mut previous = None;
    for message in messages {
        message.validate()?;
        if previous.is_some_and(|value| message.sequence <= value) {
            return Err(InvalidValue::new(
                "messages",
                "sequences are not strictly increasing",
            ));
        }
        previous = Some(message.sequence);
    }
    Ok(())
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
    fn response_families_reject_malformed_domain_values_and_page_cursors() {
        let message = serde_json::json!({
            "sequence": 1,
            "id": "msg_one",
            "squad": "alpha",
            "sender": "one",
            "recipient": "two",
            "body": "hello",
            "priority": "normal",
            "created_at": "2026-08-07T01:02:03.004Z"
        });
        let valid: MessageDto = serde_json::from_value(message.clone()).unwrap();
        assert!(valid.validate().is_ok());
        let mut invalid = valid.clone();
        invalid.sender = "INVALID".into();
        assert!(invalid.validate().is_err());

        let transcript: TranscriptResponse = serde_json::from_value(serde_json::json!({
            "messages": [message],
            "next_after": 0
        }))
        .unwrap();
        assert!(transcript.validate().is_err());

        let active_with_archive: SquadSummary = serde_json::from_value(serde_json::json!({
            "id": "sqd_one",
            "name": "alpha",
            "mission": "mission",
            "state": "active",
            "created_at": "2026-08-07T01:02:03.004Z",
            "archived_at": "2026-08-07T01:02:04.004Z"
        }))
        .unwrap();
        assert!(active_with_archive.validate().is_err());

        let invalid_roster: RosterResponse = serde_json::from_value(serde_json::json!({
            "squad": "alpha",
            "members": [{
                "membership_id": "mem_one",
                "name": "worker",
                "role": "worker",
                "membership_state": "left",
                "presence": "online",
                "availability": "idle",
                "availability_source": "agent_reported",
                "availability_observed_at": "2026-08-07T01:02:03.004Z"
            }]
        }))
        .unwrap();
        assert!(invalid_roster.validate().is_err());

        assert!(
            AckMessagesResponse {
                acknowledged_ids: vec!["bad".into()]
            }
            .validate()
            .is_err()
        );
        assert!(
            LeaveSquadResponse {
                membership_id: "bad".into(),
                left_at: serde_json::from_str("\"2026-08-07T01:02:03.004Z\"").unwrap(),
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
        assert!(
            HeartbeatRequest {
                availability: AvailabilityDto::Idle,
                availability_source: AvailabilitySourceDto::Unknown
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
            messages: (0_i64..17)
                .map(|sequence| MessageDto {
                    sequence: MessageSequence::new(sequence).unwrap(),
                    ..message.clone()
                })
                .collect(),
            pending_count: 17,
            highest_priority: Some(MessagePriorityDto::Normal),
            oldest_message_id: Some("msg_one".into()),
        };
        assert!(
            dishonest.validate().is_ok(),
            "count alone cannot detect encoded size"
        );
        let mut missing_summary = dishonest.clone();
        missing_summary.highest_priority = None;
        assert!(missing_summary.validate().is_err());
        let mut wrong_oldest = dishonest.clone();
        wrong_oldest.oldest_message_id = Some("msg_other".into());
        assert!(wrong_oldest.validate().is_err());
        assert!(encode_bounded_inbox(&dishonest).is_err());
        let honest = InboxResponse {
            messages: Vec::new(),
            pending_count: 0,
            highest_priority: None,
            oldest_message_id: None,
        };
        assert!(encode_bounded_inbox(&honest).is_ok());
        let mut dishonest_empty = honest;
        dishonest_empty.highest_priority = Some(MessagePriorityDto::High);
        assert!(dishonest_empty.validate().is_err());
    }
}
