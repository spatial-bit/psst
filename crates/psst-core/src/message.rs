use crate::{CorrelationId, MembershipId, MessageBody, MessageId, SquadId};

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum MessagePriority {
    #[default]
    Normal,
    High,
}

/// Every field that determines whether an idempotent retry is the same logical send.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageSemantics {
    pub squad: SquadId,
    pub sender: MembershipId,
    pub recipient: MembershipId,
    pub body: MessageBody,
    pub priority: MessagePriority,
    pub reply_to: Option<MessageId>,
    pub correlation_id: Option<CorrelationId>,
}

impl MessageSemantics {
    #[must_use]
    pub fn matches_retry(&self, retry: &Self) -> bool {
        self == retry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> MessageSemantics {
        MessageSemantics {
            squad: SquadId::new("sqd_alpha").unwrap(),
            sender: MembershipId::new("mem_sender").unwrap(),
            recipient: MembershipId::new("mem_recipient").unwrap(),
            body: MessageBody::new("hello").unwrap(),
            priority: MessagePriority::Normal,
            reply_to: Some(MessageId::new("msg_01").unwrap()),
            correlation_id: Some(CorrelationId::new("thread-1").unwrap()),
        }
    }

    #[test]
    fn exact_retry_matches() {
        assert!(baseline().matches_retry(&baseline()));
    }

    #[test]
    fn every_semantic_field_participates() {
        let original = baseline();
        let mut variants = Vec::new();
        let mut value = original.clone();
        value.squad = SquadId::new("sqd_beta").unwrap();
        variants.push(value);
        let mut value = original.clone();
        value.sender = MembershipId::new("mem_other").unwrap();
        variants.push(value);
        let mut value = original.clone();
        value.recipient = MembershipId::new("mem_other").unwrap();
        variants.push(value);
        let mut value = original.clone();
        value.body = MessageBody::new("different").unwrap();
        variants.push(value);
        let mut value = original.clone();
        value.priority = MessagePriority::High;
        variants.push(value);
        let mut value = original.clone();
        value.reply_to = None;
        variants.push(value);
        let mut value = original.clone();
        value.correlation_id = None;
        variants.push(value);
        assert!(
            variants
                .iter()
                .all(|variant| !original.matches_retry(variant))
        );
    }
}
