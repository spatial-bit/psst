use psst_core::{
    CorrelationId, DedupeKey, MessageBody, MessageId, MessagePriority, MessageSemantics, UnixMillis,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::{RepositoryError, Store};

/// A fully validated direct-message submission. Dedupe is mandatory at this
/// adapter-facing boundary so retry safety is the default behavior.
#[derive(Clone, Debug)]
pub struct SendMessage {
    pub id: MessageId,
    pub semantics: MessageSemantics,
    pub dedupe_key: DedupeKey,
    pub created_at: UnixMillis,
}

/// Immutable metadata returned after a durable message commit or exact retry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageRecord {
    pub sequence: i64,
    pub id: MessageId,
    pub semantics: MessageSemantics,
    pub dedupe_key: DedupeKey,
    pub created_at: UnixMillis,
}

impl Store {
    /// Atomically authorizes and commits an immutable direct message.
    ///
    /// # Errors
    ///
    /// Returns a stable lifecycle, recipient, idempotency, busy, or internal
    /// error. A successful result is never returned before the commit completes.
    pub fn send_message(
        &mut self,
        request: &SendMessage,
    ) -> Result<MessageRecord, RepositoryError> {
        self.send_message_with_commit_fault(request, false)
    }

    fn send_message_with_commit_fault(
        &mut self,
        request: &SendMessage,
        fail_after_commit: bool,
    ) -> Result<MessageRecord, RepositoryError> {
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let result = send_in_transaction(&transaction, request)?;
        transaction.commit()?;
        if fail_after_commit {
            return Err(RepositoryError::InjectedFailure);
        }
        Ok(result)
    }
}

pub(crate) fn send_in_transaction(
    transaction: &Transaction<'_>,
    request: &SendMessage,
) -> Result<MessageRecord, RepositoryError> {
    if let Some(result) = resolve_retry(transaction, request)? {
        return result;
    }

    authorize_squad_and_members(transaction, &request.semantics)?;
    authorize_reply(transaction, &request.semantics)?;

    let body_hash = Sha256::digest(request.semantics.body.as_str().as_bytes());
    transaction.execute(
        "INSERT INTO messages(
                id, squad_id, sender_membership_id, recipient_membership_id,
                body, body_hash, priority, reply_to, correlation_id, dedupe_key, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            request.id.as_str(),
            request.semantics.squad.as_str(),
            request.semantics.sender.as_str(),
            request.semantics.recipient.as_str(),
            request.semantics.body.as_str(),
            body_hash.as_slice(),
            priority_text(request.semantics.priority),
            request.semantics.reply_to.as_ref().map(MessageId::as_str),
            request
                .semantics
                .correlation_id
                .as_ref()
                .map(CorrelationId::as_str),
            request.dedupe_key.as_str(),
            request.created_at.as_i64(),
        ],
    )?;
    let sequence = transaction.last_insert_rowid();
    let result = MessageRecord {
        sequence,
        id: request.id.clone(),
        semantics: request.semantics.clone(),
        dedupe_key: request.dedupe_key.clone(),
        created_at: request.created_at,
    };
    Ok(result)
}

pub(crate) fn resolve_retry(
    transaction: &Transaction<'_>,
    request: &SendMessage,
) -> Result<Option<Result<MessageRecord, RepositoryError>>, RepositoryError> {
    Ok(find_by_dedupe(transaction, request)?.map(|existing| {
        if existing.semantics == request.semantics {
            Ok(existing)
        } else {
            Err(RepositoryError::IdempotencyConflict)
        }
    }))
}

fn authorize_squad_and_members(
    transaction: &Transaction<'_>,
    semantics: &MessageSemantics,
) -> Result<(), RepositoryError> {
    let state = transaction
        .query_row(
            "SELECT state FROM squads WHERE id = ?1",
            [semantics.squad.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match state.as_deref() {
        None => return Err(RepositoryError::NotFound),
        Some("archived") => return Err(RepositoryError::SquadArchived),
        Some("active") => {}
        Some(_) => return Err(RepositoryError::InvalidStoredData),
    }

    let sender_active: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM memberships
         WHERE id = ?1 AND squad_id = ?2 AND left_at IS NULL)",
        params![semantics.sender.as_str(), semantics.squad.as_str()],
        |row| row.get(0),
    )?;
    if !sender_active {
        return Err(RepositoryError::NotMember);
    }
    let recipient_active: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM memberships
         WHERE id = ?1 AND squad_id = ?2 AND left_at IS NULL)",
        params![semantics.recipient.as_str(), semantics.squad.as_str()],
        |row| row.get(0),
    )?;
    if !recipient_active {
        return Err(RepositoryError::RecipientNotFound);
    }
    Ok(())
}

fn authorize_reply(
    transaction: &Transaction<'_>,
    semantics: &MessageSemantics,
) -> Result<(), RepositoryError> {
    let Some(reply_to) = &semantics.reply_to else {
        return Ok(());
    };
    let valid: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE id = ?1 AND squad_id = ?2)",
        params![reply_to.as_str(), semantics.squad.as_str()],
        |row| row.get(0),
    )?;
    if valid {
        Ok(())
    } else {
        Err(RepositoryError::InvalidRequest)
    }
}

fn find_by_dedupe(
    transaction: &Transaction<'_>,
    request: &SendMessage,
) -> Result<Option<MessageRecord>, RepositoryError> {
    transaction
        .query_row(
            "SELECT sequence, id, recipient_membership_id, body, body_hash, priority,
                    reply_to, correlation_id, created_at
             FROM messages
             WHERE squad_id = ?1 AND sender_membership_id = ?2 AND dedupe_key = ?3",
            params![
                request.semantics.squad.as_str(),
                request.semantics.sender.as_str(),
                request.dedupe_key.as_str(),
            ],
            |row| {
                let body_text = row.get::<_, String>(3)?;
                let stored_hash = row.get::<_, Vec<u8>>(4)?;
                if stored_hash.as_slice() != Sha256::digest(body_text.as_bytes()).as_slice() {
                    return Err(invalid_data());
                }
                Ok(MessageRecord {
                    sequence: row.get(0)?,
                    id: parse(&row.get::<_, String>(1)?)?,
                    semantics: MessageSemantics {
                        squad: request.semantics.squad.clone(),
                        sender: request.semantics.sender.clone(),
                        recipient: parse(&row.get::<_, String>(2)?)?,
                        body: MessageBody::new(body_text).map_err(|_| invalid_data())?,
                        priority: parse_priority(&row.get::<_, String>(5)?)?,
                        reply_to: row
                            .get::<_, Option<String>>(6)?
                            .map(|value| parse(&value))
                            .transpose()?,
                        correlation_id: row
                            .get::<_, Option<String>>(7)?
                            .map(|value| parse(&value))
                            .transpose()?,
                    },
                    dedupe_key: request.dedupe_key.clone(),
                    created_at: parse_time(row.get(8)?)?,
                })
            },
        )
        .optional()
        .map_err(|error| {
            if matches!(error, rusqlite::Error::InvalidQuery) {
                RepositoryError::InvalidStoredData
            } else {
                RepositoryError::from(error)
            }
        })
}

fn priority_text(priority: MessagePriority) -> &'static str {
    match priority {
        MessagePriority::Normal => "normal",
        MessagePriority::High => "high",
    }
}

fn parse_priority(value: &str) -> rusqlite::Result<MessagePriority> {
    match value {
        "normal" => Ok(MessagePriority::Normal),
        "high" => Ok(MessagePriority::High),
        _ => Err(invalid_data()),
    }
}

fn parse<T: std::str::FromStr>(value: &str) -> rusqlite::Result<T> {
    value.parse().map_err(|_| invalid_data())
}

fn parse_time(value: i64) -> rusqlite::Result<UnixMillis> {
    UnixMillis::new(value).map_err(|_| invalid_data())
}

fn invalid_data() -> rusqlite::Error {
    rusqlite::Error::InvalidQuery
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use psst_core::{AgentId, MemberName, MembershipId, Mission, Role, SquadId, SquadName};
    use tempfile::TempDir;

    use super::*;
    use crate::{CreateSquad, JoinMembership};

    struct Fixture {
        _directory: TempDir,
        path: std::path::PathBuf,
        store: Store,
        squad: SquadId,
        sender: MembershipId,
        recipient: MembershipId,
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
                    mission: Mission::new("Test durable messages").unwrap(),
                    created_at: time(1),
                })
                .unwrap();
            let sender = join(&mut store, "sender", "one", &squad);
            let recipient = join(&mut store, "recipient", "two", &squad);
            Self {
                _directory: directory,
                path,
                store,
                squad,
                sender,
                recipient,
            }
        }

        fn request(&self, suffix: &str) -> SendMessage {
            SendMessage {
                id: MessageId::new(format!("msg_{suffix}")).unwrap(),
                semantics: MessageSemantics {
                    squad: self.squad.clone(),
                    sender: self.sender.clone(),
                    recipient: self.recipient.clone(),
                    body: MessageBody::new("hello 🦀").unwrap(),
                    priority: MessagePriority::Normal,
                    reply_to: None,
                    correlation_id: Some(CorrelationId::new("thread-1").unwrap()),
                },
                dedupe_key: DedupeKey::new(format!("send-{suffix}")).unwrap(),
                created_at: time(10),
            }
        }
    }

    fn time(value: i64) -> UnixMillis {
        UnixMillis::new(value).unwrap()
    }

    fn join(store: &mut Store, name: &str, suffix: &str, squad: &SquadId) -> MembershipId {
        join_in(store, name, suffix, squad, "alpha")
    }

    fn join_in(
        store: &mut Store,
        name: &str,
        suffix: &str,
        squad: &SquadId,
        squad_name: &str,
    ) -> MembershipId {
        let id = MembershipId::new(format!("mem_{suffix}")).unwrap();
        store
            .join(&JoinMembership {
                squad_name: SquadName::new(squad_name).unwrap(),
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

    fn row_count(store: &Store) -> i64 {
        store
            .connection
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn exact_retry_returns_original_identity_and_one_row() {
        let mut fixture = Fixture::new();
        let request = fixture.request("one");
        let original = fixture.store.send_message(&request).unwrap();
        let mut retry = request.clone();
        retry.id = MessageId::new("msg_replacement").unwrap();
        retry.created_at = time(999);
        assert_eq!(fixture.store.send_message(&retry).unwrap(), original);
        assert_eq!(row_count(&fixture.store), 1);
    }

    #[test]
    fn timeout_after_commit_retry_returns_original() {
        let mut fixture = Fixture::new();
        let request = fixture.request("ambiguous");
        assert!(matches!(
            fixture.store.send_message_with_commit_fault(&request, true),
            Err(RepositoryError::InjectedFailure)
        ));
        let retry = fixture.store.send_message(&request).unwrap();
        assert_eq!(retry.id, request.id);
        assert_eq!(retry.sequence, 1);
        assert_eq!(row_count(&fixture.store), 1);
    }

    #[test]
    fn exact_retry_survives_lifecycle_change_after_ambiguous_commit() {
        let mut fixture = Fixture::new();
        let request = fixture.request("lifecycle-retry");
        let original = fixture.store.send_message(&request).unwrap();
        fixture
            .store
            .archive_squad(&SquadName::new("alpha").unwrap(), time(20))
            .unwrap();
        assert_eq!(fixture.store.send_message(&request).unwrap(), original);
        assert_eq!(row_count(&fixture.store), 1);
    }

    #[test]
    fn every_changed_semantic_field_conflicts() {
        let mut fixture = Fixture::new();
        let mut original = fixture.request("conflict");
        let reply = fixture
            .store
            .send_message(&fixture.request("parent"))
            .unwrap();
        original.semantics.reply_to = Some(reply.id.clone());
        fixture.store.send_message(&original).unwrap();

        let mut variants = Vec::new();
        let mut changed = original.clone();
        changed.semantics.squad = SquadId::new("sqd_other").unwrap();
        variants.push(changed);
        let mut changed = original.clone();
        changed.semantics.sender = MembershipId::new("mem_other").unwrap();
        variants.push(changed);
        let mut changed = original.clone();
        changed.semantics.recipient = original.semantics.sender.clone();
        variants.push(changed);
        let mut changed = original.clone();
        changed.semantics.body = MessageBody::new("changed").unwrap();
        variants.push(changed);
        let mut changed = original.clone();
        changed.semantics.priority = MessagePriority::High;
        variants.push(changed);
        let mut changed = original.clone();
        changed.semantics.reply_to = None;
        variants.push(changed);
        let mut changed = original.clone();
        changed.semantics.correlation_id = Some(CorrelationId::new("other").unwrap());
        variants.push(changed);

        // Squad and sender changes select a different dedupe scope, so authorization
        // fails before they could collide. All fields within the same scope conflict.
        assert!(matches!(
            fixture.store.send_message(&variants[0]),
            Err(RepositoryError::NotFound)
        ));
        assert!(matches!(
            fixture.store.send_message(&variants[1]),
            Err(RepositoryError::NotMember)
        ));
        for variant in &variants[2..] {
            assert!(matches!(
                fixture.store.send_message(variant),
                Err(RepositoryError::IdempotencyConflict)
            ));
        }
        assert_eq!(row_count(&fixture.store), 2);
    }

    #[test]
    fn concurrent_duplicate_sends_across_connections_create_one_row() {
        let fixture = Fixture::new();
        let request = fixture.request("race");
        drop(fixture.store);
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = fixture.path.clone();
                let barrier = Arc::clone(&barrier);
                let request = request.clone();
                std::thread::spawn(move || {
                    let mut store = Store::open(path).unwrap();
                    barrier.wait();
                    store.send_message(&request)
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().unwrap())
            .collect();
        assert_eq!(results[0], results[1]);
        assert_eq!(row_count(&Store::open(&fixture.path).unwrap()), 1);
    }

    #[test]
    fn offline_unicode_high_priority_and_byte_boundary_are_accepted() {
        let mut fixture = Fixture::new();
        let mut request = fixture.request("boundary");
        request.semantics.body = MessageBody::new("🦀".repeat(16 * 1024)).unwrap();
        request.semantics.priority = MessagePriority::High;
        let record = fixture.store.send_message(&request).unwrap();
        assert_eq!(record.semantics.body.as_str().len(), MessageBody::MAX_BYTES);
        assert_eq!(record.semantics.priority, MessagePriority::High);
    }

    #[test]
    fn unknown_left_and_cross_squad_recipients_are_stable_errors() {
        let mut fixture = Fixture::new();
        let mut unknown = fixture.request("unknown");
        unknown.semantics.recipient = MembershipId::new("mem_unknown").unwrap();
        assert!(matches!(
            fixture.store.send_message(&unknown),
            Err(RepositoryError::RecipientNotFound)
        ));

        fixture
            .store
            .leave(&fixture.squad, &fixture.recipient, time(20))
            .unwrap();
        assert!(matches!(
            fixture.store.send_message(&fixture.request("left")),
            Err(RepositoryError::RecipientNotFound)
        ));

        fixture
            .store
            .create_squad(&CreateSquad {
                id: SquadId::new("sqd_beta").unwrap(),
                name: SquadName::new("beta").unwrap(),
                mission: Mission::new("Other").unwrap(),
                created_at: time(30),
            })
            .unwrap();
        let cross = fixture
            .store
            .join(&JoinMembership {
                squad_name: SquadName::new("beta").unwrap(),
                mission_if_missing: None,
                squad_id_if_missing: SquadId::new("sqd_beta").unwrap(),
                agent_id: AgentId::new("agt_cross").unwrap(),
                membership_id: MembershipId::new("mem_cross").unwrap(),
                member_name: MemberName::new("cross").unwrap(),
                role: Role::new("tester").unwrap(),
                joined_at: time(31),
            })
            .unwrap();
        let mut request = fixture.request("cross");
        request.semantics.recipient = cross.id;
        assert!(matches!(
            fixture.store.send_message(&request),
            Err(RepositoryError::RecipientNotFound)
        ));
    }

    #[test]
    fn left_sender_and_archived_squad_cannot_send() {
        let mut fixture = Fixture::new();
        fixture
            .store
            .leave(&fixture.squad, &fixture.sender, time(20))
            .unwrap();
        assert!(matches!(
            fixture.store.send_message(&fixture.request("left-sender")),
            Err(RepositoryError::NotMember)
        ));

        let mut fixture = Fixture::new();
        fixture
            .store
            .archive_squad(&SquadName::new("alpha").unwrap(), time(20))
            .unwrap();
        assert!(matches!(
            fixture.store.send_message(&fixture.request("archived")),
            Err(RepositoryError::SquadArchived)
        ));
    }

    #[test]
    fn reply_must_exist_in_same_squad() {
        let mut fixture = Fixture::new();
        let mut request = fixture.request("reply");
        request.semantics.reply_to = Some(MessageId::new("msg_missing").unwrap());
        assert!(matches!(
            fixture.store.send_message(&request),
            Err(RepositoryError::InvalidRequest)
        ));
    }

    #[test]
    fn existing_cross_squad_reply_is_rejected_with_stable_invalid_request() {
        let mut fixture = Fixture::new();
        let beta = SquadId::new("sqd_beta").unwrap();
        fixture
            .store
            .create_squad(&CreateSquad {
                id: beta.clone(),
                name: SquadName::new("beta").unwrap(),
                mission: Mission::new("Other squad").unwrap(),
                created_at: time(20),
            })
            .unwrap();
        let beta_sender = join_in(
            &mut fixture.store,
            "beta-sender",
            "beta-sender",
            &beta,
            "beta",
        );
        let beta_recipient = join_in(
            &mut fixture.store,
            "beta-recipient",
            "beta-recipient",
            &beta,
            "beta",
        );
        let beta_parent = fixture
            .store
            .send_message(&SendMessage {
                id: MessageId::new("msg_beta-parent").unwrap(),
                semantics: MessageSemantics {
                    squad: beta,
                    sender: beta_sender,
                    recipient: beta_recipient,
                    body: MessageBody::new("beta parent").unwrap(),
                    priority: MessagePriority::Normal,
                    reply_to: None,
                    correlation_id: None,
                },
                dedupe_key: DedupeKey::new("beta-parent").unwrap(),
                created_at: time(21),
            })
            .unwrap();
        let mut request = fixture.request("cross-squad-reply");
        request.semantics.reply_to = Some(beta_parent.id);
        let error = fixture.store.send_message(&request).unwrap_err();
        assert_eq!(error.code(), psst_core::ErrorCode::InvalidRequest);
        assert_eq!(error.to_string(), "the store request is invalid");
        assert_eq!(row_count(&fixture.store), 1);
    }

    #[test]
    fn retry_detects_body_hash_corruption_without_exposing_storage_details() {
        let mut fixture = Fixture::new();
        let request = fixture.request("hash-check");
        fixture.store.send_message(&request).unwrap();
        fixture
            .store
            .connection
            .execute(
                "UPDATE messages SET body_hash = zeroblob(32) WHERE id = ?1",
                [request.id.as_str()],
            )
            .unwrap();
        let error = fixture.store.send_message(&request).unwrap_err();
        assert!(matches!(error, RepositoryError::InvalidStoredData));
        assert_eq!(error.code(), psst_core::ErrorCode::InternalError);
        assert_eq!(error.to_string(), "the store operation failed");
    }

    #[test]
    fn restart_preserves_messages_and_sequence_is_monotonic() {
        let mut fixture = Fixture::new();
        let first = fixture
            .store
            .send_message(&fixture.request("first"))
            .unwrap();
        let second_request = fixture.request("second");
        let path = fixture.path.clone();
        drop(fixture.store);
        let mut store = Store::open(path).unwrap();
        let second = store.send_message(&second_request).unwrap();
        assert!(second.sequence > first.sequence);
        assert_eq!(row_count(&store), 2);
    }

    #[test]
    fn message_errors_are_sanitized_and_stable() {
        for (error, code, display) in [
            (
                RepositoryError::RecipientNotFound,
                psst_core::ErrorCode::RecipientNotFound,
                "the recipient membership was not found",
            ),
            (
                RepositoryError::IdempotencyConflict,
                psst_core::ErrorCode::IdempotencyConflict,
                "the dedupe key has different message semantics",
            ),
            (
                RepositoryError::PayloadTooLarge,
                psst_core::ErrorCode::PayloadTooLarge,
                "the message payload is too large",
            ),
        ] {
            assert_eq!(error.code(), code);
            assert_eq!(error.to_string(), display);
            assert!(!error.to_string().contains("messages_dedupe"));
        }
    }
}
