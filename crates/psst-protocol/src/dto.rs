use crate::ApiTimestamp;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::{IntoParams, ToSchema};

macro_rules! wire_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}

wire_enum!(SquadStateDto { Active, Archived });
wire_enum!(MembershipStateDto { Joined, Left });
wire_enum!(TransportPresenceDto { Online, Offline });
wire_enum!(AgentModeDto {
    Cooperative,
    Scheduled,
    Harnessed
});
wire_enum!(AvailabilityDto {
    Idle,
    Busy,
    Blocked,
    Unknown
});
wire_enum!(AvailabilitySourceDto {
    SessionLifecycle,
    McpConnection,
    ToolActivity,
    AgentReported,
    Unknown
});

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessagePriorityDto {
    #[default]
    Normal,
    High,
}

/// SQLite-compatible, nonnegative message sequence.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, ToSchema)]
#[serde(transparent)]
#[schema(value_type = i64)]
pub struct MessageSequence(i64);

impl MessageSequence {
    /// Creates a SQLite-compatible sequence.
    ///
    /// # Errors
    /// Returns an error for negative values.
    pub fn new(value: i64) -> Result<Self, &'static str> {
        (value >= 0)
            .then_some(Self(value))
            .ok_or("sequence must be nonnegative")
    }
    #[must_use]
    pub const fn value(self) -> i64 {
        self.0
    }
}
impl<'de> Deserialize<'de> for MessageSequence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

macro_rules! response {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
        pub struct $name { $(pub $field: $ty),* }
    };
}

response!(HealthResponse { status: String });
response!(ReadyResponse {
    status: String,
    schema_version: u32
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct SquadSummary {
    pub id: String,
    pub name: String,
    pub mission: String,
    pub state: SquadStateDto,
    pub created_at: ApiTimestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<ApiTimestamp>,
}
pub type ListSquadsResponse = Vec<SquadSummary>;
pub type GetSquadResponse = SquadSummary;
pub type CreateSquadResponse = SquadSummary;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSquadRequest {
    pub name: String,
    pub mission: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchiveSquadRequest {}
response!(ArchiveSquadResponse {
    squad: SquadSummary
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClientMetadata {
    #[schema(max_length = 64)]
    pub kind: String,
    #[serde(default)]
    #[schema(min_length = 1, max_length = 255)]
    pub hostname: Option<String>,
    #[serde(default)]
    #[schema(min_length = 1, max_length = 64)]
    pub version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinSquadRequest {
    pub name: String,
    pub role: String,
    pub mode: AgentModeDto,
    pub client: ClientMetadata,
    #[serde(default)]
    pub mission: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResumeSquadRequest {
    pub mode: AgentModeDto,
    pub client: ClientMetadata,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct SessionResponse {
    pub agent_id: String,
    pub membership_id: String,
    pub instance_id: String,
    pub squad: SquadSummary,
    pub member_name: String,
    pub role: String,
    pub heartbeat_interval_seconds: u32,
    pub lease_seconds: u32,
    pub lease_expires_at: ApiTimestamp,
}
pub type JoinSquadResponse = SessionResponse;
pub type ResumeSquadResponse = SessionResponse;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LeaveSquadRequest {}
response!(LeaveSquadResponse {
    membership_id: String,
    left_at: ApiTimestamp
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct RosterMember {
    pub membership_id: String,
    pub name: String,
    pub role: String,
    pub membership_state: MembershipStateDto,
    pub presence: TransportPresenceDto,
    pub availability: AvailabilityDto,
    pub availability_source: AvailabilitySourceDto,
    pub availability_observed_at: ApiTimestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<AgentModeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<ApiTimestamp>,
}
response!(RosterResponse { squad: String, members: Vec<RosterMember> });

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatRequest {
    pub availability: AvailabilityDto,
    pub availability_source: AvailabilitySourceDto,
}
response!(HeartbeatResponse {
    lease_expires_at: ApiTimestamp,
    heartbeat_interval_seconds: u32
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SendMessageRequest {
    pub recipient: String,
    #[schema(max_length = 65536)]
    pub body: String,
    #[serde(default)]
    pub priority: MessagePriorityDto,
    pub dedupe_key: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub correlation_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct MessageDto {
    #[schema(minimum = 0, maximum = 9223372036854775807_i64)]
    pub sequence: MessageSequence,
    pub id: String,
    pub squad: String,
    pub sender: String,
    pub recipient: String,
    pub body: String,
    pub priority: MessagePriorityDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub created_at: ApiTimestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<ApiTimestamp>,
}
response!(SendMessageResponse {
    message: MessageDto,
    idempotent_replay: bool
});

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct InboxQuery {
    #[serde(default = "default_limit")]
    #[param(minimum = 1, maximum = 100)]
    pub limit: u16,
    #[serde(default, rename = "wait")]
    #[param(rename = "wait", minimum = 0, maximum = 30)]
    pub wait_seconds: u8,
}
const fn default_limit() -> u16 {
    100
}
response!(InboxResponse {
    messages: Vec<MessageDto>,
    pending_count: u64,
    highest_priority: Option<MessagePriorityDto>,
    oldest_message_id: Option<String>
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AckMessagesRequest {
    #[schema(min_items = 1, max_items = 100)]
    pub message_ids: Vec<String>,
}
response!(AckMessagesResponse { acknowledged_ids: Vec<String> });

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct TranscriptQuery {
    #[serde(default)]
    #[param(minimum = 0, maximum = 9223372036854775807_i64)]
    pub after: MessageSequence,
    #[serde(default = "default_limit")]
    #[param(minimum = 1, maximum = 100)]
    pub limit: u16,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct TranscriptResponse {
    pub messages: Vec<MessageDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 0, maximum = 9223372036854775807_i64)]
    pub next_after: Option<MessageSequence>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutations_reject_unknown_fields_and_dedupe_is_required() {
        assert!(
            serde_json::from_str::<CreateSquadRequest>(r#"{"name":"a","mission":"m","extra":1}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<SendMessageRequest>(r#"{"recipient":"a","body":"b"}"#).is_err()
        );
    }

    #[test]
    fn sequence_is_sqlite_compatible() {
        assert!(serde_json::from_str::<MessageSequence>("-1").is_err());
        assert_eq!(
            serde_json::from_str::<MessageSequence>(&i64::MAX.to_string())
                .unwrap()
                .value(),
            i64::MAX
        );
        assert!(serde_json::from_str::<MessageSequence>("9223372036854775808").is_err());
    }

    #[test]
    fn offline_roster_omits_unknown_instance_fields() {
        let timestamp: ApiTimestamp = serde_json::from_str("\"2026-08-07T01:02:03.004Z\"").unwrap();
        let member = RosterMember {
            membership_id: "mem_one".into(),
            name: "one".into(),
            role: "critic".into(),
            membership_state: MembershipStateDto::Joined,
            presence: TransportPresenceDto::Offline,
            availability: AvailabilityDto::Unknown,
            availability_source: AvailabilitySourceDto::Unknown,
            availability_observed_at: timestamp,
            mode: None,
            last_seen_at: None,
        };
        let json = serde_json::to_string(&member).unwrap();
        assert!(!json.contains("\"mode\""));
        assert!(!json.contains("last_seen_at"));
        let decoded: RosterMember = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, member);
    }

    #[test]
    fn every_request_and_response_family_roundtrips_representative_json() {
        macro_rules! rt {
            ($ty:ty, $json:expr) => {{
                let a: $ty = serde_json::from_str($json).unwrap();
                let encoded = serde_json::to_string(&a).unwrap();
                let b: $ty = serde_json::from_str(&encoded).unwrap();
                assert_eq!(a, b, stringify!($ty));
            }};
        }
        const TS: &str = "2026-08-07T01:02:03.004Z";
        rt!(HealthResponse, r#"{"status":"ok"}"#);
        rt!(ReadyResponse, r#"{"status":"ready","schema_version":4}"#);
        let squad = format!(
            r#"{{"id":"sqd_one","name":"one","mission":"mission","state":"active","created_at":"{TS}"}}"#
        );
        rt!(SquadSummary, &squad);
        rt!(ListSquadsResponse, &format!("[{squad}]"));
        rt!(CreateSquadRequest, r#"{"name":"one","mission":"mission"}"#);
        rt!(ArchiveSquadRequest, "{}");
        rt!(ArchiveSquadResponse, &format!(r#"{{"squad":{squad}}}"#));
        rt!(
            ClientMetadata,
            r#"{"kind":"codex","hostname":"host","version":"1"}"#
        );
        rt!(
            JoinSquadRequest,
            r#"{"name":"one","role":"critic","mode":"cooperative","client":{"kind":"codex"}}"#
        );
        rt!(
            ResumeSquadRequest,
            r#"{"mode":"scheduled","client":{"kind":"claude"}}"#
        );
        let session = format!(
            r#"{{"agent_id":"agt_one","membership_id":"mem_one","instance_id":"ins_one","squad":{squad},"member_name":"one","role":"critic","heartbeat_interval_seconds":10,"lease_seconds":30,"lease_expires_at":"{TS}"}}"#
        );
        rt!(SessionResponse, &session);
        rt!(LeaveSquadRequest, "{}");
        rt!(
            LeaveSquadResponse,
            &format!(r#"{{"membership_id":"mem_one","left_at":"{TS}"}}"#)
        );
        let roster_member = format!(
            r#"{{"membership_id":"mem_one","name":"one","role":"critic","membership_state":"joined","presence":"online","availability":"busy","availability_source":"tool_activity","availability_observed_at":"{TS}","mode":"harnessed","last_seen_at":"{TS}"}}"#
        );
        rt!(RosterMember, &roster_member);
        rt!(
            RosterResponse,
            &format!(r#"{{"squad":"one","members":[{roster_member}]}}"#)
        );
        rt!(
            HeartbeatRequest,
            r#"{"availability":"idle","availability_source":"session_lifecycle"}"#
        );
        rt!(
            HeartbeatResponse,
            &format!(r#"{{"lease_expires_at":"{TS}","heartbeat_interval_seconds":10}}"#)
        );
        rt!(
            SendMessageRequest,
            r#"{"recipient":"two","body":"hello","priority":"high","dedupe_key":"send-1","reply_to":"msg_old","correlation_id":"thread"}"#
        );
        let message = format!(
            r#"{{"sequence":1,"id":"msg_one","squad":"one","sender":"one","recipient":"two","body":"hello","priority":"normal","created_at":"{TS}"}}"#
        );
        rt!(MessageDto, &message);
        rt!(
            SendMessageResponse,
            &format!(r#"{{"message":{message},"idempotent_replay":false}}"#)
        );
        rt!(InboxQuery, r#"{"limit":10,"wait":2}"#);
        rt!(
            InboxResponse,
            &format!(
                r#"{{"messages":[{message}],"pending_count":1,"highest_priority":"normal","oldest_message_id":"msg_one"}}"#
            )
        );
        rt!(AckMessagesRequest, r#"{"message_ids":["msg_one"]}"#);
        rt!(
            AckMessagesResponse,
            &format!(r#"{{"acknowledged_ids":["msg_one"],"acknowledged_at":"{TS}"}}"#)
        );
        rt!(TranscriptQuery, r#"{"after":1,"limit":10}"#);
        rt!(
            TranscriptResponse,
            &format!(r#"{{"messages":[{message}],"next_after":1}}"#)
        );
    }

    #[test]
    fn every_mutation_rejects_unknown_fields() {
        macro_rules! bad {
            ($ty:ty,$json:expr) => {
                assert!(serde_json::from_str::<$ty>($json).is_err(), stringify!($ty));
            };
        }
        bad!(
            CreateSquadRequest,
            r#"{"name":"a","mission":"m","extra":1}"#
        );
        bad!(ArchiveSquadRequest, r#"{"extra":1}"#);
        bad!(ClientMetadata, r#"{"kind":"x","extra":1}"#);
        bad!(
            JoinSquadRequest,
            r#"{"name":"a","role":"r","mode":"cooperative","client":{"kind":"x"},"extra":1}"#
        );
        bad!(
            ResumeSquadRequest,
            r#"{"mode":"cooperative","client":{"kind":"x"},"extra":1}"#
        );
        bad!(LeaveSquadRequest, r#"{"extra":1}"#);
        bad!(
            HeartbeatRequest,
            r#"{"availability":"idle","availability_source":"tool_activity","extra":1}"#
        );
        bad!(
            SendMessageRequest,
            r#"{"recipient":"a","body":"b","dedupe_key":"d","extra":1}"#
        );
        bad!(
            AckMessagesRequest,
            r#"{"message_ids":["msg_one"],"extra":1}"#
        );
    }

    #[test]
    fn every_enum_spelling_is_golden_and_unknown_values_fail() {
        macro_rules! enums { ($ty:ty,[$($variant:expr),+])=>{{ $(let value: $ty=serde_json::from_str(concat!("\"",$variant,"\"")).unwrap(); assert_eq!(serde_json::to_string(&value).unwrap(),concat!("\"",$variant,"\""));)+ assert!(serde_json::from_str::<$ty>("\"future\"").is_err()); }}; }
        enums!(SquadStateDto, ["active", "archived"]);
        enums!(MembershipStateDto, ["joined", "left"]);
        enums!(TransportPresenceDto, ["online", "offline"]);
        enums!(AgentModeDto, ["cooperative", "scheduled", "harnessed"]);
        enums!(AvailabilityDto, ["idle", "busy", "blocked", "unknown"]);
        enums!(
            AvailabilitySourceDto,
            [
                "session_lifecycle",
                "mcp_connection",
                "tool_activity",
                "agent_reported",
                "unknown"
            ]
        );
        enums!(MessagePriorityDto, ["normal", "high"]);
    }
}
