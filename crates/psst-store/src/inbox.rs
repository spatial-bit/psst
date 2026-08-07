use std::collections::HashSet;

use psst_core::{
    MembershipId, MessageBody, MessageId, MessagePriority, MessageSemantics, UnixMillis,
};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::{MessageRecord, RepositoryError, Store};

pub const MAX_INBOX_MESSAGES: usize = 100;
pub const MAX_INBOX_OUTPUT_BYTES: usize = 1024 * 1024;
pub const MAX_ACK_MESSAGES: usize = 100;

/// A bounded read of one membership's authoritative unacknowledged inbox.
#[derive(Clone, Debug)]
pub struct InboxQuery {
    pub recipient: MembershipId,
    pub limit: usize,
}

/// An atomic acknowledgement of explicitly enumerated messages.
#[derive(Clone, Debug)]
pub struct AcknowledgeMessages {
    pub recipient: MembershipId,
    pub message_ids: Vec<MessageId>,
    pub acknowledged_at: UnixMillis,
}

impl Store {
    /// Returns an ascending sequence prefix of the recipient's pending inbox.
    ///
    /// Retrieval never acknowledges messages. The returned prefix is bounded by
    /// both the requested count and a conservative worst-case JSON size estimate.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError::InvalidRequest`] unless `limit` is in `1..=100`,
    /// or a stable storage error if stored message data cannot be decoded.
    pub fn pending_inbox(&self, query: &InboxQuery) -> Result<Vec<MessageRecord>, RepositoryError> {
        pending_inbox_on(&self.connection, query)
    }

    /// Atomically acknowledges messages owned by one recipient.
    ///
    /// # Errors
    ///
    /// Returns a stable validation, ownership, busy, or storage error.
    pub fn acknowledge_messages(
        &mut self,
        request: &AcknowledgeMessages,
    ) -> Result<(), RepositoryError> {
        self.acknowledge_messages_with_fault(request, false)
    }

    fn acknowledge_messages_with_fault(
        &mut self,
        request: &AcknowledgeMessages,
        fail_after_first_update: bool,
    ) -> Result<(), RepositoryError> {
        if !(1..=MAX_ACK_MESSAGES).contains(&request.message_ids.len()) {
            return Err(RepositoryError::InvalidRequest);
        }
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        acknowledge_on(&transaction, request, fail_after_first_update)?;
        transaction.commit()?;
        Ok(())
    }
}

pub(crate) fn pending_inbox_on(
    connection: &Connection,
    query: &InboxQuery,
) -> Result<Vec<MessageRecord>, RepositoryError> {
    if !(1..=MAX_INBOX_MESSAGES).contains(&query.limit) {
        return Err(RepositoryError::InvalidRequest);
    }
    let mut statement = connection.prepare(
        "SELECT sequence, id, squad_id, sender_membership_id,
                    recipient_membership_id, body, body_hash, priority, reply_to,
                    correlation_id, dedupe_key, created_at
             FROM messages
             WHERE recipient_membership_id = ?1 AND acknowledged_at IS NULL
             ORDER BY sequence ASC
             LIMIT ?2",
    )?;
    let rows = statement.query_map(params![query.recipient.as_str(), query.limit], map_message)?;
    let mut messages = Vec::new();
    let mut estimated_bytes = 2_usize; // JSON array brackets.
    for row in rows {
        let message = row.map_err(map_decode_error)?;
        let item_bytes = conservative_serialized_size(&message);
        let separator = usize::from(!messages.is_empty());
        if estimated_bytes
            .checked_add(separator)
            .and_then(|size| size.checked_add(item_bytes))
            .is_none_or(|size| size > MAX_INBOX_OUTPUT_BYTES)
        {
            break;
        }
        estimated_bytes += separator + item_bytes;
        messages.push(message);
    }
    Ok(messages)
}

pub(crate) fn acknowledge_on(
    transaction: &Transaction<'_>,
    request: &AcknowledgeMessages,
    fail_after_first_update: bool,
) -> Result<(), RepositoryError> {
    let mut distinct = HashSet::with_capacity(request.message_ids.len());
    for id in &request.message_ids {
        if !distinct.insert(id.clone()) {
            continue;
        }
        let owned: bool = transaction.query_row(
            "SELECT EXISTS(
                    SELECT 1 FROM messages
                    WHERE id = ?1 AND recipient_membership_id = ?2
                )",
            params![id.as_str(), request.recipient.as_str()],
            |row| row.get(0),
        )?;
        if !owned {
            return Err(RepositoryError::NotFound);
        }
    }
    for (index, id) in distinct.into_iter().enumerate() {
        transaction.execute(
            "UPDATE messages SET acknowledged_at = ?3
                 WHERE id = ?1 AND recipient_membership_id = ?2
                   AND acknowledged_at IS NULL",
            params![
                id.as_str(),
                request.recipient.as_str(),
                request.acknowledged_at.as_i64()
            ],
        )?;
        if fail_after_first_update && index == 0 {
            return Err(RepositoryError::InjectedFailure);
        }
    }
    Ok(())
}

pub(crate) fn map_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRecord> {
    let body = row.get::<_, String>(5)?;
    let stored_hash = row.get::<_, Vec<u8>>(6)?;
    if stored_hash.as_slice() != Sha256::digest(body.as_bytes()).as_slice() {
        return Err(invalid_data());
    }
    Ok(MessageRecord {
        sequence: row.get(0)?,
        id: parse(&row.get::<_, String>(1)?)?,
        semantics: MessageSemantics {
            squad: parse(&row.get::<_, String>(2)?)?,
            sender: parse(&row.get::<_, String>(3)?)?,
            recipient: parse(&row.get::<_, String>(4)?)?,
            body: MessageBody::new(body).map_err(|_| invalid_data())?,
            priority: match row.get::<_, String>(7)?.as_str() {
                "normal" => MessagePriority::Normal,
                "high" => MessagePriority::High,
                _ => return Err(invalid_data()),
            },
            reply_to: row
                .get::<_, Option<String>>(8)?
                .map(|value| parse(&value))
                .transpose()?,
            correlation_id: row
                .get::<_, Option<String>>(9)?
                .map(|value| parse(&value))
                .transpose()?,
        },
        dedupe_key: parse(&row.get::<_, String>(10)?)?,
        created_at: UnixMillis::new(row.get(11)?).map_err(|_| invalid_data())?,
    })
}

fn conservative_serialized_size(message: &MessageRecord) -> usize {
    const STRUCTURAL_OVERHEAD: usize = 512;
    let variable_bytes = message.id.as_str().len()
        + message.semantics.squad.as_str().len()
        + message.semantics.sender.as_str().len()
        + message.semantics.recipient.as_str().len()
        + message.semantics.body.as_str().len()
        + message.dedupe_key.as_str().len()
        + message
            .semantics
            .reply_to
            .as_ref()
            .map_or(0, |value| value.as_str().len())
        + message
            .semantics
            .correlation_id
            .as_ref()
            .map_or(0, |value| value.as_str().len());
    // A JSON string byte can expand to six ASCII bytes as a `\u00XX` escape.
    variable_bytes
        .saturating_mul(6)
        .saturating_add(STRUCTURAL_OVERHEAD)
}

fn parse<T: std::str::FromStr>(value: &str) -> rusqlite::Result<T> {
    value.parse().map_err(|_| invalid_data())
}

fn invalid_data() -> rusqlite::Error {
    rusqlite::Error::InvalidQuery
}

fn map_decode_error(error: rusqlite::Error) -> RepositoryError {
    if matches!(error, rusqlite::Error::InvalidQuery) {
        RepositoryError::InvalidStoredData
    } else {
        RepositoryError::from(error)
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::Duration;

    use psst_core::{
        AgentId, CorrelationId, DedupeKey, MemberName, Mission, Role, SquadId, SquadName,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::{CreateSquad, JoinMembership, SendMessage};

    struct Fixture {
        _directory: TempDir,
        path: std::path::PathBuf,
        store: Store,
        squad: SquadId,
        sender: MembershipId,
        recipient: MembershipId,
        foreign_recipient: MembershipId,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = TempDir::new().unwrap();
            let path = directory.path().join("psst.db");
            let mut store = Store::open(&path).unwrap();
            let squad = SquadId::new("sqd_alpha").unwrap();
            store
                .create_squad(&CreateSquad {
                    id: squad.clone(),
                    name: SquadName::new("alpha").unwrap(),
                    mission: Mission::new("Test inbox durability").unwrap(),
                    created_at: time(1),
                })
                .unwrap();
            let sender = join(&mut store, &squad, "sender", "one");
            let recipient = join(&mut store, &squad, "recipient", "two");
            let foreign_recipient = join(&mut store, &squad, "foreign", "three");
            Self {
                _directory: directory,
                path,
                store,
                squad,
                sender,
                recipient,
                foreign_recipient,
            }
        }

        fn send(
            &mut self,
            suffix: &str,
            recipient: MembershipId,
            body: String,
            priority: MessagePriority,
        ) -> MessageRecord {
            self.store
                .send_message(&SendMessage {
                    id: MessageId::new(format!("msg_{suffix}")).unwrap(),
                    semantics: MessageSemantics {
                        squad: self.squad.clone(),
                        sender: self.sender.clone(),
                        recipient,
                        body: MessageBody::new(body).unwrap(),
                        priority,
                        reply_to: None,
                        correlation_id: Some(CorrelationId::new("thread-1").unwrap()),
                    },
                    dedupe_key: DedupeKey::new(format!("send-{suffix}")).unwrap(),
                    created_at: time(10),
                })
                .unwrap()
        }

        fn inbox(&self, recipient: &MembershipId, limit: usize) -> Vec<MessageRecord> {
            self.store
                .pending_inbox(&InboxQuery {
                    recipient: recipient.clone(),
                    limit,
                })
                .unwrap()
        }
    }

    fn time(value: i64) -> UnixMillis {
        UnixMillis::new(value).unwrap()
    }

    fn join(store: &mut Store, squad: &SquadId, name: &str, suffix: &str) -> MembershipId {
        let id = MembershipId::new(format!("mem_{suffix}")).unwrap();
        store
            .join(&JoinMembership {
                squad_name: SquadName::new("alpha").unwrap(),
                mission_if_missing: None,
                squad_id_if_missing: squad.clone(),
                agent_id: AgentId::new(format!("agt_{suffix}")).unwrap(),
                membership_id: id.clone(),
                member_name: MemberName::new(name).unwrap(),
                role: Role::new("tester").unwrap(),
                joined_at: time(2),
            })
            .unwrap();
        id
    }

    #[test]
    fn repeated_reads_and_restart_replay_without_acknowledging() {
        let mut fixture = Fixture::new();
        let recipient = fixture.recipient.clone();
        fixture.send(
            "one",
            recipient.clone(),
            "first".into(),
            MessagePriority::Normal,
        );
        fixture.send(
            "two",
            recipient.clone(),
            "second".into(),
            MessagePriority::High,
        );
        let first = fixture.inbox(&recipient, 100);
        assert_eq!(fixture.inbox(&recipient, 100), first);
        let path = fixture.path.clone();
        drop(fixture.store);
        fixture.store = Store::open(path).unwrap();
        assert_eq!(fixture.inbox(&recipient, 100), first);
        let acknowledged: i64 = fixture
            .store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE acknowledged_at IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(acknowledged, 0);
    }

    #[test]
    fn sequence_order_ignores_priority_and_limit_is_acknowledgement_driven() {
        let mut fixture = Fixture::new();
        let recipient = fixture.recipient.clone();
        let normal = fixture.send(
            "normal",
            recipient.clone(),
            "first".into(),
            MessagePriority::Normal,
        );
        let high = fixture.send(
            "high",
            recipient.clone(),
            "second".into(),
            MessagePriority::High,
        );
        assert_eq!(fixture.inbox(&recipient, 1), vec![normal.clone()]);
        assert_eq!(fixture.inbox(&recipient, 1), vec![normal.clone()]);
        fixture
            .store
            .acknowledge_messages(&AcknowledgeMessages {
                recipient: recipient.clone(),
                message_ids: vec![normal.id],
                acknowledged_at: time(20),
            })
            .unwrap();
        assert_eq!(fixture.inbox(&recipient, 1), vec![high]);
    }

    #[test]
    fn pending_query_uses_sequence_index_without_temporary_sort() {
        let fixture = Fixture::new();
        let mut statement = fixture
            .store
            .connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT sequence, id, squad_id, sender_membership_id,
                        recipient_membership_id, body, body_hash, priority, reply_to,
                        correlation_id, dedupe_key, created_at
                 FROM messages
                 WHERE recipient_membership_id = ?1 AND acknowledged_at IS NULL
                 ORDER BY sequence ASC
                 LIMIT ?2",
            )
            .unwrap();
        let details = statement
            .query_map(params![fixture.recipient.as_str(), 100], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("USING INDEX messages_inbox_order")),
            "unexpected query plan: {details:?}"
        );
        assert!(
            details.iter().all(|detail| !detail.contains("TEMP B-TREE")),
            "query requires a temporary sort: {details:?}"
        );
    }

    #[test]
    fn acknowledgement_is_durable_and_idempotent() {
        let mut fixture = Fixture::new();
        let recipient = fixture.recipient.clone();
        let message = fixture.send(
            "ack",
            recipient.clone(),
            "mail".into(),
            MessagePriority::Normal,
        );
        let request = AcknowledgeMessages {
            recipient: recipient.clone(),
            message_ids: vec![message.id.clone(), message.id.clone()],
            acknowledged_at: time(20),
        };
        fixture.store.acknowledge_messages(&request).unwrap();
        fixture.store.acknowledge_messages(&request).unwrap();
        let path = fixture.path.clone();
        drop(fixture.store);
        fixture.store = Store::open(path).unwrap();
        assert!(fixture.inbox(&recipient, 100).is_empty());
        let persisted: i64 = fixture
            .store
            .connection
            .query_row(
                "SELECT acknowledged_at FROM messages WHERE id = ?1",
                [message.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted, 20);
    }

    #[test]
    fn mixed_unknown_or_foreign_batch_rolls_back_all_updates() {
        for invalid_id in [
            MessageId::new("msg_unknown").unwrap(),
            MessageId::new("msg_foreign").unwrap(),
        ] {
            let mut fixture = Fixture::new();
            let recipient = fixture.recipient.clone();
            let valid = fixture.send(
                "valid",
                recipient.clone(),
                "valid".into(),
                MessagePriority::Normal,
            );
            if invalid_id.as_str() == "msg_foreign" {
                let foreign = fixture.foreign_recipient.clone();
                fixture.send(
                    "foreign",
                    foreign,
                    "foreign".into(),
                    MessagePriority::Normal,
                );
            }
            let error = fixture
                .store
                .acknowledge_messages(&AcknowledgeMessages {
                    recipient: recipient.clone(),
                    message_ids: vec![valid.id.clone(), invalid_id],
                    acknowledged_at: time(20),
                })
                .unwrap_err();
            assert!(matches!(error, RepositoryError::NotFound));
            assert_eq!(fixture.inbox(&recipient, 100), vec![valid]);
        }
    }

    #[test]
    fn count_and_conservative_aggregate_bounds_are_enforced() {
        let mut fixture = Fixture::new();
        let recipient = fixture.recipient.clone();
        for index in 0..101 {
            fixture.send(
                &format!("small-{index}"),
                recipient.clone(),
                "x".into(),
                MessagePriority::Normal,
            );
        }
        assert_eq!(fixture.inbox(&recipient, 100).len(), 100);

        let mut fixture = Fixture::new();
        let recipient = fixture.recipient.clone();
        for index in 0..4 {
            fixture.send(
                &format!("large-{index}"),
                recipient.clone(),
                "\u{0001}".repeat(MessageBody::MAX_BYTES),
                MessagePriority::Normal,
            );
        }
        let inbox = fixture.inbox(&recipient, 100);
        assert!(!inbox.is_empty());
        assert!(inbox.len() < 4);
        let estimate = 2
            + inbox
                .iter()
                .map(conservative_serialized_size)
                .sum::<usize>()
            + inbox.len().saturating_sub(1);
        assert!(estimate <= MAX_INBOX_OUTPUT_BYTES);
    }

    #[test]
    fn invalid_limits_and_empty_ack_batches_are_rejected() {
        let mut fixture = Fixture::new();
        for limit in [0, 101] {
            assert!(matches!(
                fixture.store.pending_inbox(&InboxQuery {
                    recipient: fixture.recipient.clone(),
                    limit
                }),
                Err(RepositoryError::InvalidRequest)
            ));
        }
        assert!(matches!(
            fixture.store.acknowledge_messages(&AcknowledgeMessages {
                recipient: fixture.recipient.clone(),
                message_ids: Vec::new(),
                acknowledged_at: time(20),
            }),
            Err(RepositoryError::InvalidRequest)
        ));
    }

    #[test]
    fn oversized_ack_batch_is_rejected_without_mutation() {
        let mut fixture = Fixture::new();
        let recipient = fixture.recipient.clone();
        let message = fixture.send(
            "ack-bound",
            recipient.clone(),
            "mail".into(),
            MessagePriority::Normal,
        );
        let error = fixture
            .store
            .acknowledge_messages(&AcknowledgeMessages {
                recipient: recipient.clone(),
                message_ids: vec![message.id.clone(); MAX_ACK_MESSAGES + 1],
                acknowledged_at: time(20),
            })
            .unwrap_err();
        assert!(matches!(error, RepositoryError::InvalidRequest));
        assert_eq!(fixture.inbox(&recipient, 100), vec![message]);
    }

    #[test]
    fn inbox_rejects_corrupted_message_content() {
        let mut fixture = Fixture::new();
        let recipient = fixture.recipient.clone();
        let message = fixture.send(
            "corrupt",
            recipient.clone(),
            "original".into(),
            MessagePriority::Normal,
        );
        fixture
            .store
            .connection
            .execute(
                "UPDATE messages SET body = 'tampered' WHERE id = ?1",
                [message.id.as_str()],
            )
            .unwrap();
        assert!(matches!(
            fixture.store.pending_inbox(&InboxQuery {
                recipient,
                limit: 100,
            }),
            Err(RepositoryError::InvalidStoredData)
        ));
    }

    #[test]
    fn busy_writer_returns_stable_error_without_mutation_then_recovers() {
        let mut fixture = Fixture::new();
        let recipient = fixture.recipient.clone();
        let message = fixture.send(
            "busy",
            recipient.clone(),
            "mail".into(),
            MessagePriority::Normal,
        );
        let mut contender = Store::open(&fixture.path).unwrap();
        contender
            .connection
            .busy_timeout(Duration::from_millis(20))
            .unwrap();
        let lock = fixture
            .store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let request = AcknowledgeMessages {
            recipient: recipient.clone(),
            message_ids: vec![message.id.clone()],
            acknowledged_at: time(20),
        };
        let error = contender.acknowledge_messages(&request).unwrap_err();
        assert!(matches!(error, RepositoryError::DatabaseBusy));
        assert_eq!(error.code(), psst_core::ErrorCode::DatabaseBusy);
        assert_eq!(
            contender
                .pending_inbox(&InboxQuery {
                    recipient: recipient.clone(),
                    limit: 100,
                })
                .unwrap(),
            vec![message]
        );
        drop(lock);
        contender.acknowledge_messages(&request).unwrap();
        assert!(
            contender
                .pending_inbox(&InboxQuery {
                    recipient,
                    limit: 100,
                })
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn injected_mid_batch_ack_failure_rolls_back_across_reopen() {
        let mut fixture = Fixture::new();
        let recipient = fixture.recipient.clone();
        let first = fixture.send(
            "fault-one",
            recipient.clone(),
            "first".into(),
            MessagePriority::Normal,
        );
        let second = fixture.send(
            "fault-two",
            recipient.clone(),
            "second".into(),
            MessagePriority::Normal,
        );
        let error = fixture
            .store
            .acknowledge_messages_with_fault(
                &AcknowledgeMessages {
                    recipient: recipient.clone(),
                    message_ids: vec![first.id.clone(), second.id.clone()],
                    acknowledged_at: time(20),
                },
                true,
            )
            .unwrap_err();
        assert!(matches!(error, RepositoryError::InjectedFailure));
        let path = fixture.path.clone();
        drop(fixture.store);
        fixture.store = Store::open(path).unwrap();
        assert_eq!(fixture.inbox(&recipient, 100), vec![first, second]);
        let acknowledged: i64 = fixture
            .store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE acknowledged_at IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(acknowledged, 0);
    }

    #[test]
    fn abrupt_death_before_and_after_ack_preserves_commit_boundary() {
        for (mode, should_replay) in [("before", true), ("after", false)] {
            let mut fixture = Fixture::new();
            let recipient = fixture.recipient.clone();
            let message = fixture.send(
                &format!("abrupt-{mode}"),
                recipient.clone(),
                "mail".into(),
                MessagePriority::Normal,
            );
            let output = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("inbox::tests::abrupt_death_child")
                .arg("--nocapture")
                .env("PSST_TEST_ABRUPT_DB", &fixture.path)
                .env("PSST_TEST_ABRUPT_MODE", mode)
                .env("PSST_TEST_ABRUPT_RECIPIENT", recipient.as_str())
                .env("PSST_TEST_ABRUPT_MESSAGE", message.id.as_str())
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(23),
                "child did not exit at the failpoint: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let path = fixture.path.clone();
            drop(fixture.store);
            fixture.store = Store::open(path).unwrap();
            let pending = fixture.inbox(&recipient, 100);
            if should_replay {
                assert_eq!(pending, vec![message]);
            } else {
                assert!(pending.is_empty());
            }
        }
    }

    #[test]
    fn abrupt_death_child() {
        let Ok(path) = std::env::var("PSST_TEST_ABRUPT_DB") else {
            return;
        };
        let mode = std::env::var("PSST_TEST_ABRUPT_MODE").unwrap();
        let recipient =
            MembershipId::new(std::env::var("PSST_TEST_ABRUPT_RECIPIENT").unwrap()).unwrap();
        let message = MessageId::new(std::env::var("PSST_TEST_ABRUPT_MESSAGE").unwrap()).unwrap();
        let mut store = Store::open(path).unwrap();
        let pending = store
            .pending_inbox(&InboxQuery {
                recipient: recipient.clone(),
                limit: 100,
            })
            .unwrap();
        assert_eq!(pending.len(), 1);
        match mode.as_str() {
            "before" => {}
            "after" => store
                .acknowledge_messages(&AcknowledgeMessages {
                    recipient,
                    message_ids: vec![message],
                    acknowledged_at: time(20),
                })
                .unwrap(),
            _ => panic!("unknown abrupt-death mode"),
        }
        std::process::exit(23);
    }
}
