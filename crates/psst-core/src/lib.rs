//! I/O-free domain vocabulary and state rules for Psst.

#![forbid(unsafe_code)]

mod error;
mod id;
mod message;
mod secret;
mod state;
mod time;
mod value;

pub use error::{DomainError, ErrorCode, InvalidValue};
pub use id::{AgentId, InstanceId, MembershipId, MessageId, SquadId};
pub use message::{MessagePriority, MessageSemantics};
pub use secret::ResumeToken;
pub use state::{
    AgentMode, Availability, AvailabilityObservation, AvailabilitySource, InstanceState,
    MembershipState, MessageState, SquadState, renew_lease,
};
pub use time::{Clock, UnixMillis};
pub use value::{CorrelationId, DedupeKey, MemberName, MessageBody, Mission, Role, SquadName};

/// The protocol version implemented by this workspace.
pub const PROTOCOL_VERSION: &str = "v1";
