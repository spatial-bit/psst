use psst_core::{
    AgentMode, Availability, AvailabilityObservation, AvailabilitySource, CorrelationId, DedupeKey,
    InstanceId, InstanceState, MemberName, MembershipId, MessageBody, MessageId, MessagePriority,
    MessageSemantics, ResumeToken, SquadId, SquadName, SquadState, UnixMillis, renew_lease,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::inbox::{acknowledge_on, map_message, pending_inbox_on};
use crate::instance::{authenticate, insert_instance, validate_client, validate_observation};
use crate::message::{find_by_dedupe, resolve_retry, send_in_transaction};
use crate::{
    AcknowledgeMessages, ClaimOutcome, InboxQuery, InstanceRecord, JoinMembership, LeasePolicy,
    MembershipRecord, MessageRecord, RepositoryError, RosterMember, SendMessage, SquadRecord,
    Store,
};

#[derive(Clone, Debug)]
pub struct SendByName {
    pub id: MessageId,
    pub recipient: MemberName,
    pub body: MessageBody,
    pub priority: MessagePriority,
    pub dedupe_key: DedupeKey,
    pub reply_to: Option<MessageId>,
    pub correlation_id: Option<CorrelationId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageView {
    pub message: MessageRecord,
    pub squad: SquadName,
    pub sender: MemberName,
    pub recipient: MemberName,
    pub acknowledged_at: Option<UnixMillis>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SendOutcome {
    pub message: MessageView,
    pub idempotent_replay: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboxPage {
    pub messages: Vec<MessageView>,
    pub pending_count: u64,
    pub highest_priority: Option<MessagePriority>,
    pub oldest_message_id: Option<MessageId>,
    /// Authenticated recipient identity used by the relay for local wake routing.
    /// This metadata is never serialized on the wire.
    pub recipient_membership: MembershipId,
}

#[derive(Clone, Debug)]
pub struct TranscriptByName {
    pub squad: SquadName,
    pub after: i64,
    pub limit: usize,
}

#[cfg(test)]
use psst_core::{AgentId, Mission, Role};
#[cfg(test)]
use std::{
    sync::{Arc, Barrier},
    time::Duration,
};
#[cfg(test)]
use tempfile::TempDir;

/// Adapter-owned credential. It is intentionally neither serializable nor printable.
pub struct AuthenticatedSession<'a> {
    pub instance_id: &'a InstanceId,
    pub resume_token: &'a ResumeToken,
    pub now: UnixMillis,
}

pub struct JoinAndClaim<'a> {
    pub membership: JoinMembership,
    pub instance_id: InstanceId,
    pub mode: AgentMode,
    pub client_kind: &'a str,
    pub hostname: Option<&'a str>,
    pub availability: Availability,
    pub availability_source: AvailabilitySource,
    pub lease_policy: LeasePolicy,
}

pub struct JoinAndClaimOutcome {
    pub membership: MembershipRecord,
    pub squad: SquadRecord,
    claim: ClaimOutcome,
}

impl JoinAndClaimOutcome {
    #[must_use]
    pub const fn claim(&self) -> &ClaimOutcome {
        &self.claim
    }

    #[must_use]
    pub fn into_parts(self) -> (MembershipRecord, InstanceRecord, ResumeToken) {
        let (instance, token) = self.claim.into_parts();
        (self.membership, instance, token)
    }
    #[must_use]
    pub fn into_session_parts(
        self,
    ) -> (MembershipRecord, SquadRecord, InstanceRecord, ResumeToken) {
        let (instance, token) = self.claim.into_parts();
        (self.membership, self.squad, instance, token)
    }
}

#[derive(Debug)]
pub struct SessionContext {
    pub membership: MembershipRecord,
    pub squad: SquadRecord,
    pub instance: InstanceRecord,
}

#[derive(Debug)]
pub struct LeaveOutcome {
    pub membership_id: MembershipId,
    pub left_at: UnixMillis,
}

#[cfg(test)]
struct AuthenticatedRaceTests;

#[cfg(test)]
fn race_millis(value: i64) -> UnixMillis {
    UnixMillis::new(value).unwrap()
}

#[cfg(test)]
fn race_join(suffix: &str, name: &str, now: i64) -> JoinAndClaim<'static> {
    JoinAndClaim {
        membership: JoinMembership {
            squad_name: SquadName::new("alpha").unwrap(),
            mission_if_missing: Some(Mission::new("Authenticated collaboration").unwrap()),
            squad_id_if_missing: SquadId::new("sqd_alpha").unwrap(),
            agent_id: AgentId::new(format!("agt_{suffix}")).unwrap(),
            membership_id: MembershipId::new(format!("mem_{suffix}")).unwrap(),
            member_name: MemberName::new(name).unwrap(),
            role: Role::new("engineer").unwrap(),
            joined_at: race_millis(now),
        },
        instance_id: InstanceId::new(format!("ins_{suffix}")).unwrap(),
        mode: AgentMode::Cooperative,
        client_kind: "test",
        hostname: None,
        availability: Availability::Unknown,
        availability_source: AvailabilitySource::Unknown,
        lease_policy: LeasePolicy::new(Duration::from_millis(10), Duration::from_millis(30))
            .unwrap(),
    }
}

#[cfg(test)]
fn race_message(
    suffix: &str,
    sender: &MembershipRecord,
    recipient: &MembershipRecord,
    now: i64,
) -> SendMessage {
    SendMessage {
        id: MessageId::new(format!("msg_{suffix}")).unwrap(),
        semantics: MessageSemantics {
            squad: sender.squad_id.clone(),
            sender: sender.id.clone(),
            recipient: recipient.id.clone(),
            body: MessageBody::new(format!("body {suffix}")).unwrap(),
            priority: MessagePriority::Normal,
            reply_to: None,
            correlation_id: None,
        },
        dedupe_key: DedupeKey::new(format!("dedupe-{suffix}")).unwrap(),
        created_at: race_millis(now),
    }
}

#[cfg(test)]
impl AuthenticatedRaceTests {
    fn concurrent_send_and_archive_have_only_serializable_outcomes() {
        for round in 0..8 {
            let directory = TempDir::new().unwrap();
            let path = directory.path().join("psst.db");
            let mut setup = Store::open(&path).unwrap();
            let (alice, instance, token) = setup
                .join_and_claim(&race_join("alice", "alice", 100))
                .unwrap()
                .into_parts();
            let (bob, _, _) = setup
                .join_and_claim(&race_join("bob", "bob", 101))
                .unwrap()
                .into_parts();
            drop(setup);
            let barrier = Arc::new(Barrier::new(2));
            let (send_path, send_barrier, send_instance, send_token) = (
                path.clone(),
                Arc::clone(&barrier),
                instance.id.clone(),
                token.clone(),
            );
            let outbound = race_message(&format!("race-{round}"), &alice, &bob, 110);
            let sender = std::thread::spawn(move || {
                let mut store = Store::open(send_path).unwrap();
                let session = AuthenticatedSession {
                    instance_id: &send_instance,
                    resume_token: &send_token,
                    now: race_millis(110),
                };
                send_barrier.wait();
                store.authenticated_send(&session, &outbound)
            });
            let (archive_path, archive_barrier, archive_instance, archive_token, squad) = (
                path.clone(),
                Arc::clone(&barrier),
                instance.id.clone(),
                token.clone(),
                alice.squad_id.clone(),
            );
            let archiver = std::thread::spawn(move || {
                let mut store = Store::open(archive_path).unwrap();
                let session = AuthenticatedSession {
                    instance_id: &archive_instance,
                    resume_token: &archive_token,
                    now: race_millis(110),
                };
                archive_barrier.wait();
                store.authenticated_archive(&session, &squad)
            });
            let sent = sender.join().unwrap();
            archiver.join().unwrap().unwrap();
            assert!(
                sent.is_ok() || matches!(sent, Err(RepositoryError::SquadArchived)),
                "round {round}: {sent:?}"
            );
            let store = Store::open(path).unwrap();
            let count: i64 = store
                .connection
                .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, i64::from(sent.is_ok()), "round {round}");
        }
    }

    fn concurrent_leave_and_resume_preserve_single_membership_owner() {
        for round in 0..8 {
            let directory = TempDir::new().unwrap();
            let path = directory.path().join("psst.db");
            let mut setup = Store::open(&path).unwrap();
            let (member, instance, token) = setup
                .join_and_claim(&race_join("alice", "alice", 100))
                .unwrap()
                .into_parts();
            drop(setup);
            let barrier = Arc::new(Barrier::new(2));
            let (leave_path, leave_barrier, leave_instance, leave_token) = (
                path.clone(),
                Arc::clone(&barrier),
                instance.id.clone(),
                token.clone(),
            );
            let leave = std::thread::spawn(move || {
                let mut store = Store::open(leave_path).unwrap();
                let session = AuthenticatedSession {
                    instance_id: &leave_instance,
                    resume_token: &leave_token,
                    now: race_millis(129),
                };
                leave_barrier.wait();
                store.authenticated_leave(&session, &SquadName::new("alpha").unwrap())
            });
            let (resume_path, resume_barrier, resume_instance, resume_token) = (
                path.clone(),
                Arc::clone(&barrier),
                instance.id.clone(),
                token.clone(),
            );
            let resume = std::thread::spawn(move || {
                let mut store = Store::open(resume_path).unwrap();
                resume_barrier.wait();
                store.authenticated_resume(
                    &resume_instance,
                    &resume_token,
                    InstanceId::new(format!("ins_resumed{round}")).unwrap(),
                    AgentMode::Cooperative,
                    "test",
                    None,
                    Availability::Unknown,
                    AvailabilitySource::Unknown,
                    race_millis(130),
                    LeasePolicy::new(Duration::from_millis(10), Duration::from_millis(30)).unwrap(),
                    &SquadName::new("alpha").unwrap(),
                )
            });
            let left = leave.join().unwrap();
            let resumed = resume.join().unwrap();
            assert_ne!(
                left.is_ok(),
                resumed.is_ok(),
                "round {round}: leave={left:?}, resume={resumed:?}"
            );
            let store = Store::open(path).unwrap();
            let (left_at, live): (Option<i64>, i64) = store.connection.query_row("SELECT m.left_at,(SELECT COUNT(*) FROM instances i WHERE i.membership_id=m.id AND i.closed_at IS NULL) FROM memberships m WHERE m.id=?1", [member.id.as_str()], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
            assert_eq!(
                (left_at.is_some(), live),
                (left.is_ok(), i64::from(resumed.is_ok())),
                "round {round}"
            );
        }
    }

    fn concurrent_resume_and_send_never_send_as_replacement_owner() {
        for round in 0..8 {
            let directory = TempDir::new().unwrap();
            let path = directory.path().join("psst.db");
            let mut setup = Store::open(&path).unwrap();
            let (alice, instance, token) = setup
                .join_and_claim(&race_join("alice", "alice", 100))
                .unwrap()
                .into_parts();
            let (bob, _, _) = setup
                .join_and_claim(&race_join("bob", "bob", 101))
                .unwrap()
                .into_parts();
            drop(setup);
            let barrier = Arc::new(Barrier::new(2));
            let (send_path, send_barrier, send_instance, send_token) = (
                path.clone(),
                Arc::clone(&barrier),
                instance.id.clone(),
                token.clone(),
            );
            let outbound = race_message(&format!("resume-send-{round}"), &alice, &bob, 129);
            let send_thread = std::thread::spawn(move || {
                let mut store = Store::open(send_path).unwrap();
                let session = AuthenticatedSession {
                    instance_id: &send_instance,
                    resume_token: &send_token,
                    now: race_millis(129),
                };
                send_barrier.wait();
                store.authenticated_send(&session, &outbound)
            });
            let (resume_path, resume_barrier, resume_instance, resume_token) = (
                path.clone(),
                Arc::clone(&barrier),
                instance.id.clone(),
                token.clone(),
            );
            let resume = std::thread::spawn(move || {
                let mut store = Store::open(resume_path).unwrap();
                resume_barrier.wait();
                store.authenticated_resume(
                    &resume_instance,
                    &resume_token,
                    InstanceId::new(format!("ins_new{round}")).unwrap(),
                    AgentMode::Cooperative,
                    "test",
                    None,
                    Availability::Unknown,
                    AvailabilitySource::Unknown,
                    race_millis(130),
                    LeasePolicy::new(Duration::from_millis(10), Duration::from_millis(30)).unwrap(),
                    &SquadName::new("alpha").unwrap(),
                )
            });
            let sent = send_thread.join().unwrap();
            assert!(resume.join().unwrap().is_ok(), "round {round}");
            assert!(
                sent.is_ok() || matches!(sent, Err(RepositoryError::NotMember)),
                "round {round}: {sent:?}"
            );
            let store = Store::open(path).unwrap();
            let count: i64 = store
                .connection
                .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, i64::from(sent.is_ok()), "round {round}");
        }
    }
}

pub struct TranscriptQuery {
    pub squad: SquadId,
    pub after: i64,
    pub limit: usize,
}

struct Identity {
    membership: MembershipId,
    squad: SquadId,
    mode: AgentMode,
    expiry: UnixMillis,
    last_seen: UnixMillis,
    created_at: UnixMillis,
}

type ResumeAuthorityRow = (Vec<u8>, String, Option<i64>, i64, String, String);

// Every method returns the same stable RepositoryError surface documented on
// RepositoryError; repeating that list on nine adjacent command boundaries
// would obscure the transaction semantics.
#[allow(clippy::missing_errors_doc)]
impl Store {
    /// Creates squad/agent/membership and the first owner in one commit.
    pub fn join_and_claim(
        &mut self,
        request: &JoinAndClaim<'_>,
    ) -> Result<JoinAndClaimOutcome, RepositoryError> {
        self.join_and_claim_inner(request, None)
    }

    fn join_and_claim_inner(
        &mut self,
        request: &JoinAndClaim<'_>,
        fault: Option<JoinClaimFault>,
    ) -> Result<JoinAndClaimOutcome, RepositoryError> {
        validate_client(request.client_kind, request.hostname)?;
        validate_observation(
            request.availability,
            request.availability_source,
            request.membership.joined_at,
        )?;
        let token = ResumeToken::generate().map_err(|_| RepositoryError::EntropyUnavailable)?;
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let squad = find_or_create_squad(&tx, &request.membership)?;
        inject_join_claim_fault(fault, JoinClaimFault::Squad)?;
        tx.execute(
            "INSERT INTO agents(id, created_at) VALUES (?1, ?2)",
            params![
                request.membership.agent_id.as_str(),
                request.membership.joined_at.as_i64()
            ],
        )?;
        inject_join_claim_fault(fault, JoinClaimFault::Agent)?;
        let changed = tx.execute(
            "INSERT INTO memberships(id,squad_id,agent_id,name,normalized_name,role,joined_at)
             VALUES (?1,?2,?3,?4,?4,?5,?6)
             ON CONFLICT(squad_id, normalized_name) WHERE left_at IS NULL DO NOTHING",
            params![
                request.membership.membership_id.as_str(),
                squad.as_str(),
                request.membership.agent_id.as_str(),
                request.membership.member_name.as_str(),
                request.membership.role.as_str(),
                request.membership.joined_at.as_i64()
            ],
        )?;
        if changed == 0 {
            return Err(RepositoryError::NameInUse);
        }
        inject_join_claim_fault(fault, JoinClaimFault::Membership)?;
        let instance = insert_instance(
            &tx,
            request.instance_id.clone(),
            request.membership.membership_id.clone(),
            request.mode,
            request.client_kind,
            request.hostname,
            &token,
            request.availability,
            request.availability_source,
            request.membership.joined_at,
            request.lease_policy,
        )?;
        inject_join_claim_fault(fault, JoinClaimFault::Instance)?;
        let squad_record = squad_by_id(&tx, &squad)?;
        tx.commit()?;
        let membership = MembershipRecord {
            id: request.membership.membership_id.clone(),
            squad_id: squad,
            agent_id: request.membership.agent_id.clone(),
            name: request.membership.member_name.clone(),
            role: request.membership.role.clone(),
            joined_at: request.membership.joined_at,
            left_at: None,
        };
        Ok(JoinAndClaimOutcome {
            membership,
            squad: squad_record,
            claim: ClaimOutcome::new(instance, token),
        })
    }

    pub fn authenticated_heartbeat(
        &mut self,
        session: &AuthenticatedSession<'_>,
        availability: Availability,
        source: AvailabilitySource,
        policy: LeasePolicy,
    ) -> Result<InstanceRecord, RepositoryError> {
        AvailabilityObservation::new(availability, source, session.now)
            .map_err(|_| RepositoryError::InvalidRequest)?;
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity = authorize_current(&tx, session)?;
        let effective_now = session.now.max(identity.last_seen);
        let expiry = renew_lease(
            InstanceState::at(None, identity.expiry, effective_now),
            identity.expiry,
            effective_now,
            policy.lease_duration(),
        )
        .map_err(|_| RepositoryError::LeaseExpired)?;
        tx.execute("UPDATE instances SET availability=?2, availability_source=?3, availability_observed_at=?4, lease_expires_at=?5, last_seen_at=?4 WHERE id=?1", params![session.instance_id.as_str(), availability_name(availability), source_name(source), effective_now.as_i64(), expiry.as_i64()])?;
        tx.commit()?;
        Ok(InstanceRecord {
            id: session.instance_id.clone(),
            membership_id: identity.membership,
            mode: identity.mode,
            availability,
            availability_source: source,
            availability_observed_at: effective_now,
            lease_expires_at: expiry,
            last_seen_at: effective_now,
            created_at: identity.created_at,
            closed_at: None,
            heartbeat_interval: policy.heartbeat_interval(),
            lease_duration: policy.lease_duration(),
        })
    }

    /// Reads the roster only when the credential belongs to the named squad.
    ///
    /// Historical authentication deliberately permits a former member to read
    /// preserved roster history after leaving or after the squad is archived,
    /// while preventing a profile bound to another squad from selecting it by
    /// name.
    pub fn authenticated_roster(
        &mut self,
        session: &AuthenticatedSession<'_>,
        expected_squad: &SquadName,
    ) -> Result<Vec<RosterMember>, RepositoryError> {
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let identity = authenticate_historical(&tx, session)?;
        let squad = squad_by_id(&tx, &identity.squad)?;
        if &squad.name != expected_squad {
            return Err(RepositoryError::NotFound);
        }
        tx.commit()?;
        self.roster(expected_squad, session.now)
    }

    pub fn authenticated_send(
        &mut self,
        session: &AuthenticatedSession<'_>,
        request: &SendMessage,
    ) -> Result<MessageRecord, RepositoryError> {
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity = authenticate_historical(&tx, session)?;
        if request.semantics.sender != identity.membership
            || request.semantics.squad != identity.squad
        {
            return Err(RepositoryError::NotMember);
        }
        if let Some(result) = resolve_retry(&tx, request)? {
            let result = result?;
            tx.commit()?;
            return Ok(result);
        }
        authorize_current(&tx, session)?;
        let result = send_in_transaction(&tx, request)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn authenticated_send_by_name(
        &mut self,
        session: &AuthenticatedSession<'_>,
        request: &SendByName,
    ) -> Result<SendOutcome, RepositoryError> {
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity = authenticate_historical(&tx, session)?;

        let existing_recipient = tx
            .query_row(
                "SELECT recipient_membership_id FROM messages
                 WHERE squad_id = ?1 AND sender_membership_id = ?2 AND dedupe_key = ?3",
                params![
                    identity.squad.as_str(),
                    identity.membership.as_str(),
                    request.dedupe_key.as_str()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| RepositoryError::InvalidStoredData)
            })
            .transpose()?;

        if let Some(recipient_id) = existing_recipient {
            let candidate = send_from_name(
                request,
                &identity.squad,
                &identity.membership,
                recipient_id,
                session.now,
            );
            let existing =
                find_by_dedupe(&tx, &candidate)?.ok_or(RepositoryError::InvalidStoredData)?;
            if existing.semantics != candidate.semantics {
                return Err(RepositoryError::IdempotencyConflict);
            }
            let view = message_view_on(&tx, existing)?;
            if view.recipient != request.recipient {
                return Err(RepositoryError::IdempotencyConflict);
            }
            tx.commit()?;
            return Ok(SendOutcome {
                message: view,
                idempotent_replay: true,
            });
        }

        authorize_current(&tx, session)?;
        let recipient_id = tx
            .query_row(
                "SELECT id FROM memberships
                 WHERE squad_id = ?1 AND normalized_name = ?2 AND left_at IS NULL",
                params![identity.squad.as_str(), request.recipient.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(RepositoryError::RecipientNotFound)?
            .parse()
            .map_err(|_| RepositoryError::InvalidStoredData)?;
        let send = send_from_name(
            request,
            &identity.squad,
            &identity.membership,
            recipient_id,
            session.now,
        );
        let record = send_in_transaction(&tx, &send)?;
        let view = message_view_on(&tx, record)?;
        tx.commit()?;
        Ok(SendOutcome {
            message: view,
            idempotent_replay: false,
        })
    }

    pub fn authenticated_pending_page(
        &mut self,
        session: &AuthenticatedSession<'_>,
        limit: usize,
    ) -> Result<InboxPage, RepositoryError> {
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let identity = authorize_current(&tx, session)?;
        let pending_count = tx.query_row(
            "SELECT COUNT(*) FROM messages
             WHERE recipient_membership_id = ?1 AND acknowledged_at IS NULL",
            [identity.membership.as_str()],
            |row| row.get::<_, u64>(0),
        )?;
        let highest_priority = tx
            .query_row(
                "SELECT priority FROM messages
                 WHERE recipient_membership_id = ?1 AND acknowledged_at IS NULL
                 ORDER BY CASE priority WHEN 'high' THEN 1 ELSE 0 END DESC
                 LIMIT 1",
                [identity.membership.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| match value.as_str() {
                "normal" => Ok(MessagePriority::Normal),
                "high" => Ok(MessagePriority::High),
                _ => Err(RepositoryError::InvalidStoredData),
            })
            .transpose()?;
        let oldest_message_id = tx
            .query_row(
                "SELECT id FROM messages
                 WHERE recipient_membership_id = ?1 AND acknowledged_at IS NULL
                 ORDER BY sequence ASC LIMIT 1",
                [identity.membership.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| MessageId::new(value).map_err(|_| RepositoryError::InvalidStoredData))
            .transpose()?;
        let records = pending_inbox_on(
            &tx,
            &InboxQuery {
                recipient: identity.membership.clone(),
                limit,
            },
        )?;
        let messages = records
            .into_iter()
            .map(|record| message_view_on(&tx, record))
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit()?;
        Ok(InboxPage {
            messages,
            pending_count,
            highest_priority,
            oldest_message_id,
            recipient_membership: identity.membership,
        })
    }

    pub fn authenticated_pending(
        &mut self,
        session: &AuthenticatedSession<'_>,
        limit: usize,
    ) -> Result<Vec<MessageRecord>, RepositoryError> {
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let identity = authorize_current(&tx, session)?;
        let result = pending_inbox_on(
            &tx,
            &InboxQuery {
                recipient: identity.membership,
                limit,
            },
        )?;
        tx.commit()?;
        Ok(result)
    }

    pub fn authenticated_acknowledge(
        &mut self,
        session: &AuthenticatedSession<'_>,
        message_ids: Vec<MessageId>,
    ) -> Result<(), RepositoryError> {
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity = authorize_current(&tx, session)?;
        acknowledge_on(
            &tx,
            &AcknowledgeMessages {
                recipient: identity.membership,
                message_ids,
                acknowledged_at: session.now,
            },
            false,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn authenticated_transcript(
        &mut self,
        session: &AuthenticatedSession<'_>,
        query: &TranscriptQuery,
    ) -> Result<Vec<MessageRecord>, RepositoryError> {
        if !(1..=100).contains(&query.limit) || query.after < 0 {
            return Err(RepositoryError::InvalidRequest);
        }
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let identity = authorize_current(&tx, session)?;
        if identity.squad != query.squad {
            return Err(RepositoryError::NotMember);
        }
        let result = {
            let mut statement = tx.prepare("SELECT sequence,id,squad_id,sender_membership_id,recipient_membership_id,body,body_hash,priority,reply_to,correlation_id,dedupe_key,created_at FROM messages WHERE squad_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3")?;
            statement
                .query_map(
                    params![query.squad.as_str(), query.after, query.limit],
                    map_message,
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        tx.commit()?;
        Ok(result)
    }

    pub fn authenticated_transcript_views(
        &mut self,
        session: &AuthenticatedSession<'_>,
        query: &TranscriptByName,
    ) -> Result<Vec<MessageView>, RepositoryError> {
        if !(1..=100).contains(&query.limit) || query.after < 0 {
            return Err(RepositoryError::InvalidRequest);
        }
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let identity = authorize_current(&tx, session)?;
        let squad = squad_by_id(&tx, &identity.squad)?;
        if squad.name != query.squad {
            return Err(RepositoryError::NotMember);
        }
        let records = {
            let mut statement = tx.prepare("SELECT sequence,id,squad_id,sender_membership_id,recipient_membership_id,body,body_hash,priority,reply_to,correlation_id,dedupe_key,created_at FROM messages WHERE squad_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3")?;
            statement
                .query_map(
                    params![identity.squad.as_str(), query.after, query.limit],
                    map_message,
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let views = records
            .into_iter()
            .map(|record| message_view_on(&tx, record))
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit()?;
        Ok(views)
    }

    pub fn authenticated_leave(
        &mut self,
        session: &AuthenticatedSession<'_>,
        expected_squad: &SquadName,
    ) -> Result<LeaveOutcome, RepositoryError> {
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity = authorize_current(&tx, session)?;
        let squad = squad_by_id(&tx, &identity.squad)?;
        if &squad.name != expected_squad {
            return Err(RepositoryError::NotFound);
        }
        tx.execute(
            "UPDATE instances SET closed_at=?2 WHERE id=?1 AND closed_at IS NULL",
            params![session.instance_id.as_str(), session.now.as_i64()],
        )?;
        tx.execute(
            "UPDATE memberships SET left_at=?2 WHERE id=?1 AND left_at IS NULL",
            params![identity.membership.as_str(), session.now.as_i64()],
        )?;
        tx.commit()?;
        Ok(LeaveOutcome {
            membership_id: identity.membership,
            left_at: session.now,
        })
    }

    pub fn authenticated_archive(
        &mut self,
        session: &AuthenticatedSession<'_>,
        squad: &SquadId,
    ) -> Result<(), RepositoryError> {
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity = authorize_current(&tx, session)?;
        if &identity.squad != squad {
            return Err(RepositoryError::NotMember);
        }
        let changed = tx.execute(
            "UPDATE squads SET state='archived', archived_at=?2 WHERE id=?1 AND state='active'",
            params![squad.as_str(), session.now.as_i64()],
        )?;
        if changed == 0 {
            return Err(RepositoryError::SquadArchived);
        }
        tx.commit()?;
        Ok(())
    }

    pub fn authenticated_archive_by_name(
        &mut self,
        session: &AuthenticatedSession<'_>,
        squad_name: &SquadName,
    ) -> Result<SquadRecord, RepositoryError> {
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity = authorize_current(&tx, session)?;
        let squad = squad_by_name(&tx, squad_name)?;
        if identity.squad != squad.id {
            return Err(RepositoryError::NotMember);
        }
        if squad.state == SquadState::Archived {
            return Err(RepositoryError::SquadArchived);
        }
        tx.execute(
            "UPDATE squads SET state='archived', archived_at=?2 WHERE id=?1 AND state='active'",
            params![squad.id.as_str(), session.now.as_i64()],
        )?;
        let result = SquadRecord {
            state: SquadState::Archived,
            archived_at: Some(session.now),
            ..squad
        };
        tx.commit()?;
        Ok(result)
    }

    /// Rotates an expired/closed instance while retaining the continuity secret.
    #[allow(clippy::too_many_arguments)]
    pub fn authenticated_resume(
        &mut self,
        prior_instance: &InstanceId,
        token: &ResumeToken,
        new_instance: InstanceId,
        mode: AgentMode,
        client_kind: &str,
        hostname: Option<&str>,
        availability: Availability,
        source: AvailabilitySource,
        now: UnixMillis,
        policy: LeasePolicy,
        expected_squad: &SquadName,
    ) -> Result<SessionContext, RepositoryError> {
        validate_client(client_kind, hostname)?;
        validate_observation(availability, source, now)?;
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: Option<ResumeAuthorityRow> = tx.query_row(
            "SELECT i.resume_token_hash,i.membership_id,i.closed_at,i.lease_expires_at,s.name,s.state
             FROM instances i JOIN memberships m ON m.id=i.membership_id
             JOIN squads s ON s.id=m.squad_id WHERE i.id=?1 AND m.left_at IS NULL",
            [prior_instance.as_str()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).optional()?;
        let Some((hash, membership, closed, expiry, squad_name, squad_state)) = prior else {
            return Err(RepositoryError::NotFound);
        };
        authenticate(&hash, token).map_err(|_| RepositoryError::NotFound)?;
        if squad_name != expected_squad.as_str() {
            return Err(RepositoryError::NotFound);
        }
        if squad_state == "archived" {
            return Err(RepositoryError::SquadArchived);
        }
        if squad_state != "active" {
            return Err(RepositoryError::InvalidStoredData);
        }
        if closed.is_none() && now.as_i64() < expiry {
            return Err(RepositoryError::NameInUse);
        }
        let membership =
            MembershipId::new(membership).map_err(|_| RepositoryError::InvalidStoredData)?;
        let has_live_owner: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM instances
                 WHERE membership_id=?1 AND closed_at IS NULL AND lease_expires_at>?2
             )",
            params![membership.as_str(), now.as_i64()],
            |row| row.get(0),
        )?;
        if has_live_owner {
            return Err(RepositoryError::NameInUse);
        }
        tx.execute("UPDATE instances SET closed_at=COALESCE(closed_at,?2) WHERE membership_id=?1 AND closed_at IS NULL", params![membership.as_str(), now.as_i64()])?;
        let membership_record = membership_by_id(&tx, &membership)?;
        let squad = squad_by_id(&tx, &membership_record.squad_id)?;
        let result = insert_instance(
            &tx,
            new_instance,
            membership,
            mode,
            client_kind,
            hostname,
            token,
            availability,
            source,
            now,
            policy,
        )?;
        tx.commit()?;
        Ok(SessionContext {
            membership: membership_record,
            squad,
            instance: result,
        })
    }
}

fn send_from_name(
    request: &SendByName,
    squad: &SquadId,
    sender: &MembershipId,
    recipient: MembershipId,
    created_at: UnixMillis,
) -> SendMessage {
    SendMessage {
        id: request.id.clone(),
        semantics: MessageSemantics {
            squad: squad.clone(),
            sender: sender.clone(),
            recipient,
            body: request.body.clone(),
            priority: request.priority,
            reply_to: request.reply_to.clone(),
            correlation_id: request.correlation_id.clone(),
        },
        dedupe_key: request.dedupe_key.clone(),
        created_at,
    }
}

fn message_view_on(
    tx: &Transaction<'_>,
    message: MessageRecord,
) -> Result<MessageView, RepositoryError> {
    let (squad, sender, recipient, acknowledged_at) = tx
        .query_row(
            "SELECT s.name, sm.name, rm.name, m.acknowledged_at
             FROM messages m
             JOIN squads s ON s.id = m.squad_id
             JOIN memberships sm ON sm.id = m.sender_membership_id
             JOIN memberships rm ON rm.id = m.recipient_membership_id
             WHERE m.id = ?1",
            [message.id.as_str()],
            |row| {
                Ok((
                    parse_value(&row.get::<_, String>(0)?)?,
                    parse_value(&row.get::<_, String>(1)?)?,
                    parse_value(&row.get::<_, String>(2)?)?,
                    optional_timestamp_value(row.get(3)?)?,
                ))
            },
        )
        .optional()?
        .ok_or(RepositoryError::InvalidStoredData)?;
    Ok(MessageView {
        message,
        squad,
        sender,
        recipient,
        acknowledged_at,
    })
}

fn squad_by_name(tx: &Transaction<'_>, name: &SquadName) -> Result<SquadRecord, RepositoryError> {
    tx.query_row(
        "SELECT id,name,mission,state,created_at,archived_at FROM squads WHERE name=?1",
        [name.as_str()],
        map_squad_row,
    )
    .optional()?
    .ok_or(RepositoryError::NotFound)
}
fn squad_by_id(tx: &Transaction<'_>, id: &SquadId) -> Result<SquadRecord, RepositoryError> {
    tx.query_row(
        "SELECT id,name,mission,state,created_at,archived_at FROM squads WHERE id=?1",
        [id.as_str()],
        map_squad_row,
    )
    .optional()?
    .ok_or(RepositoryError::NotFound)
}
fn map_squad_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SquadRecord> {
    let state: String = row.get(3)?;
    Ok(SquadRecord {
        id: parse_value(&row.get::<_, String>(0)?)?,
        name: parse_value(&row.get::<_, String>(1)?)?,
        mission: parse_value(&row.get::<_, String>(2)?)?,
        state: match state.as_str() {
            "active" => SquadState::Active,
            "archived" => SquadState::Archived,
            _ => return Err(invalid_data()),
        },
        created_at: timestamp_value(row.get(4)?)?,
        archived_at: optional_timestamp_value(row.get(5)?)?,
    })
}
fn membership_by_id(
    tx: &Transaction<'_>,
    id: &MembershipId,
) -> Result<MembershipRecord, RepositoryError> {
    tx.query_row(
        "SELECT id,squad_id,agent_id,name,role,joined_at,left_at FROM memberships WHERE id=?1",
        [id.as_str()],
        |r| {
            Ok(MembershipRecord {
                id: parse_value(&r.get::<_, String>(0)?)?,
                squad_id: parse_value(&r.get::<_, String>(1)?)?,
                agent_id: parse_value(&r.get::<_, String>(2)?)?,
                name: parse_value(&r.get::<_, String>(3)?)?,
                role: parse_value(&r.get::<_, String>(4)?)?,
                joined_at: timestamp_value(r.get(5)?)?,
                left_at: optional_timestamp_value(r.get(6)?)?,
            })
        },
    )
    .optional()?
    .ok_or(RepositoryError::NotFound)
}
fn parse_value<T: std::str::FromStr>(value: &str) -> rusqlite::Result<T> {
    value.parse().map_err(|_| invalid_data())
}
fn timestamp_value(value: i64) -> rusqlite::Result<UnixMillis> {
    UnixMillis::new(value).map_err(|_| invalid_data())
}
fn optional_timestamp_value(value: Option<i64>) -> rusqlite::Result<Option<UnixMillis>> {
    value.map(timestamp_value).transpose()
}
fn invalid_data() -> rusqlite::Error {
    rusqlite::Error::InvalidQuery
}

fn authenticate_historical(
    tx: &Transaction<'_>,
    session: &AuthenticatedSession<'_>,
) -> Result<Identity, RepositoryError> {
    #[allow(clippy::type_complexity)]
    let row: Option<(Vec<u8>, String, String, String, i64, i64, i64)> = tx.query_row(
        "SELECT i.resume_token_hash,m.id,m.squad_id,i.mode,i.lease_expires_at,i.last_seen_at,i.created_at FROM instances i JOIN memberships m ON m.id=i.membership_id WHERE i.id=?1",
        [session.instance_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
    ).optional()?;
    let Some((hash, membership, squad, mode, expiry, last_seen, created_at)) = row else {
        return Err(RepositoryError::NotFound);
    };
    authenticate(&hash, session.resume_token).map_err(|_| RepositoryError::NotFound)?;
    Ok(Identity {
        membership: MembershipId::new(membership)
            .map_err(|_| RepositoryError::InvalidStoredData)?,
        squad: SquadId::new(squad).map_err(|_| RepositoryError::InvalidStoredData)?,
        mode: parse_mode(&mode)?,
        expiry: UnixMillis::new(expiry).map_err(|_| RepositoryError::InvalidStoredData)?,
        last_seen: UnixMillis::new(last_seen).map_err(|_| RepositoryError::InvalidStoredData)?,
        created_at: UnixMillis::new(created_at).map_err(|_| RepositoryError::InvalidStoredData)?,
    })
}

fn authorize_current(
    tx: &Transaction<'_>,
    session: &AuthenticatedSession<'_>,
) -> Result<Identity, RepositoryError> {
    let identity = authenticate_historical(tx, session)?;
    // A valid credential gets the stable lease-expired result even when a later
    // lifecycle transition (leave/archive/replacement) would also reject it.
    // Unknown instance IDs and wrong tokens remain concealed by the historical
    // authentication boundary above.
    if session.now >= identity.expiry {
        return Err(RepositoryError::LeaseExpired);
    }
    let (closed, left, squad_state): (Option<i64>, Option<i64>, String) = tx.query_row(
        "SELECT i.closed_at,m.left_at,s.state
         FROM instances i
         JOIN memberships m ON m.id=i.membership_id
         JOIN squads s ON s.id=m.squad_id
         WHERE i.id=?1",
        [session.instance_id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if squad_state == "archived" {
        return Err(RepositoryError::SquadArchived);
    }
    if squad_state != "active" {
        return Err(RepositoryError::InvalidStoredData);
    }
    if closed.is_some() || left.is_some() {
        return Err(RepositoryError::NotMember);
    }
    Ok(identity)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum JoinClaimFault {
    Squad,
    Agent,
    Membership,
    Instance,
}

fn inject_join_claim_fault(
    actual: Option<JoinClaimFault>,
    point: JoinClaimFault,
) -> Result<(), RepositoryError> {
    if actual == Some(point) {
        Err(RepositoryError::InjectedFailure)
    } else {
        Ok(())
    }
}

fn find_or_create_squad(
    tx: &Transaction<'_>,
    request: &JoinMembership,
) -> Result<SquadId, RepositoryError> {
    if let Some((id, state)) = tx
        .query_row(
            "SELECT id,state FROM squads WHERE name=?1",
            [request.squad_name.as_str()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if state != "active" {
            return Err(RepositoryError::SquadArchived);
        }
        return SquadId::new(id).map_err(|_| RepositoryError::InvalidStoredData);
    }
    let mission = request
        .mission_if_missing
        .as_ref()
        .ok_or(RepositoryError::NotFound)?;
    tx.execute(
        "INSERT INTO squads(id,name,mission,state,created_at) VALUES (?1,?2,?3,'active',?4)",
        params![
            request.squad_id_if_missing.as_str(),
            request.squad_name.as_str(),
            mission.as_str(),
            request.joined_at.as_i64()
        ],
    )?;
    Ok(request.squad_id_if_missing.clone())
}

fn parse_mode(value: &str) -> Result<AgentMode, RepositoryError> {
    match value {
        "cooperative" => Ok(AgentMode::Cooperative),
        "scheduled" => Ok(AgentMode::Scheduled),
        "harnessed" => Ok(AgentMode::Harnessed),
        _ => Err(RepositoryError::InvalidStoredData),
    }
}
const fn availability_name(value: Availability) -> &'static str {
    match value {
        Availability::Idle => "idle",
        Availability::Busy => "busy",
        Availability::Blocked => "blocked",
        Availability::Unknown => "unknown",
    }
}
const fn source_name(value: AvailabilitySource) -> &'static str {
    match value {
        AvailabilitySource::SessionLifecycle => "session_lifecycle",
        AvailabilitySource::McpConnection => "mcp_connection",
        AvailabilitySource::ToolActivity => "tool_activity",
        AvailabilitySource::AgentReported => "agent_reported",
        AvailabilitySource::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use psst_core::{
        AgentId, CorrelationId, DedupeKey, MemberName, MessageBody, MessagePriority,
        MessageSemantics, Mission, Role, SquadName,
    };
    use tempfile::TempDir;

    use super::*;

    fn millis(value: i64) -> UnixMillis {
        UnixMillis::new(value).unwrap()
    }

    fn join_request(suffix: &str, name: &str, now: i64) -> JoinAndClaim<'static> {
        JoinAndClaim {
            membership: JoinMembership {
                squad_name: SquadName::new("alpha").unwrap(),
                mission_if_missing: Some(Mission::new("Authenticated collaboration").unwrap()),
                squad_id_if_missing: SquadId::new("sqd_alpha").unwrap(),
                agent_id: AgentId::new(format!("agt_{suffix}")).unwrap(),
                membership_id: MembershipId::new(format!("mem_{suffix}")).unwrap(),
                member_name: MemberName::new(name).unwrap(),
                role: Role::new("engineer").unwrap(),
                joined_at: millis(now),
            },
            instance_id: InstanceId::new(format!("ins_{suffix}")).unwrap(),
            mode: AgentMode::Cooperative,
            client_kind: "test",
            hostname: None,
            availability: Availability::Unknown,
            availability_source: AvailabilitySource::Unknown,
            lease_policy: LeasePolicy::new(Duration::from_millis(10), Duration::from_millis(30))
                .unwrap(),
        }
    }

    fn message(
        suffix: &str,
        sender: &MembershipRecord,
        recipient: &MembershipRecord,
        now: i64,
    ) -> SendMessage {
        SendMessage {
            id: MessageId::new(format!("msg_{suffix}")).unwrap(),
            semantics: MessageSemantics {
                squad: sender.squad_id.clone(),
                sender: sender.id.clone(),
                recipient: recipient.id.clone(),
                body: MessageBody::new(format!("body {suffix}")).unwrap(),
                priority: MessagePriority::Normal,
                reply_to: None,
                correlation_id: None,
            },
            dedupe_key: DedupeKey::new(format!("dedupe-{suffix}")).unwrap(),
            created_at: millis(now),
        }
    }

    #[test]
    fn bootstrap_is_atomic_and_authentication_is_concealed() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        let request = join_request("alice", "alice", 100);
        let outcome = store.join_and_claim(&request).unwrap();
        let (_, instance, token) = outcome.into_parts();
        let wrong =
            ResumeToken::from_encoded("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let wrong_session = AuthenticatedSession {
            instance_id: &instance.id,
            resume_token: &wrong,
            now: millis(110),
        };
        assert!(matches!(
            store.authenticated_pending(&wrong_session, 10),
            Err(RepositoryError::NotFound)
        ));
        let expired = AuthenticatedSession {
            instance_id: &instance.id,
            resume_token: &token,
            now: millis(130),
        };
        assert!(matches!(
            store.authenticated_pending(&expired, 10),
            Err(RepositoryError::LeaseExpired)
        ));
    }

    #[test]
    fn authenticated_sender_and_mailbox_are_derived_from_session() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        let alice = store
            .join_and_claim(&join_request("alice", "alice", 100))
            .unwrap();
        let bob = store
            .join_and_claim(&join_request("bob", "bob", 101))
            .unwrap();
        let (alice_member, alice_instance, alice_token) = alice.into_parts();
        let (bob_member, _, _) = bob.into_parts();
        let session = AuthenticatedSession {
            instance_id: &alice_instance.id,
            resume_token: &alice_token,
            now: millis(110),
        };
        let valid = SendMessage {
            id: MessageId::new("msg_valid").unwrap(),
            semantics: MessageSemantics {
                squad: alice_member.squad_id.clone(),
                sender: alice_member.id.clone(),
                recipient: bob_member.id.clone(),
                body: MessageBody::new("hello").unwrap(),
                priority: MessagePriority::Normal,
                reply_to: None,
                correlation_id: Some(CorrelationId::new("thread").unwrap()),
            },
            dedupe_key: DedupeKey::new("send-valid").unwrap(),
            created_at: millis(110),
        };
        store.authenticated_send(&session, &valid).unwrap();
        let mut forged = valid.clone();
        forged.id = MessageId::new("msg_forged").unwrap();
        forged.dedupe_key = DedupeKey::new("send-forged").unwrap();
        forged.semantics.sender = bob_member.id;
        assert!(matches!(
            store.authenticated_send(&session, &forged),
            Err(RepositoryError::NotMember)
        ));

        store
            .authenticated_archive(&session, &alice_member.squad_id)
            .unwrap();
        assert_eq!(
            store.authenticated_send(&session, &valid).unwrap().id,
            valid.id
        );
        let mut new = valid.clone();
        new.id = MessageId::new("msg_new").unwrap();
        new.dedupe_key = DedupeKey::new("send-new").unwrap();
        assert!(matches!(
            store.authenticated_send(&session, &new),
            Err(RepositoryError::SquadArchived)
        ));
        let mut changed = valid.clone();
        changed.semantics.body = MessageBody::new("changed").unwrap();
        assert!(matches!(
            store.authenticated_send(&session, &changed),
            Err(RepositoryError::IdempotencyConflict)
        ));
    }

    #[test]
    fn inbox_activation_metadata_covers_mail_beyond_returned_page() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        let alice = store
            .join_and_claim(&join_request("alice", "alice", 100))
            .unwrap();
        let bob = store
            .join_and_claim(&join_request("bob", "bob", 101))
            .unwrap();
        let (alice_member, alice_instance, alice_token) = alice.into_parts();
        let (bob_member, bob_instance, bob_token) = bob.into_parts();
        let alice_session = AuthenticatedSession {
            instance_id: &alice_instance.id,
            resume_token: &alice_token,
            now: millis(110),
        };
        for index in 0..101 {
            let mut value = message(
                &format!("activation-{index}"),
                &alice_member,
                &bob_member,
                110,
            );
            if index == 100 {
                value.semantics.priority = MessagePriority::High;
            }
            store.authenticated_send(&alice_session, &value).unwrap();
        }
        let bob_session = AuthenticatedSession {
            instance_id: &bob_instance.id,
            resume_token: &bob_token,
            now: millis(110),
        };
        let page = store.authenticated_pending_page(&bob_session, 1).unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.pending_count, 101);
        assert_eq!(page.highest_priority, Some(MessagePriority::High));
        assert_eq!(
            page.oldest_message_id.as_ref().map(MessageId::as_str),
            Some("msg_activation-0")
        );
        assert_eq!(page.messages[0].message.id.as_str(), "msg_activation-0");
    }

    #[test]
    fn expired_credential_can_repeat_exact_commit_but_cannot_change_or_send_new() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        let alice = store
            .join_and_claim(&join_request("alice", "alice", 100))
            .unwrap();
        let bob = store
            .join_and_claim(&join_request("bob", "bob", 101))
            .unwrap();
        let (alice_member, instance, token) = alice.into_parts();
        let (bob_member, _, _) = bob.into_parts();
        let active = AuthenticatedSession {
            instance_id: &instance.id,
            resume_token: &token,
            now: millis(110),
        };
        let message = SendMessage {
            id: MessageId::new("msg_expiry").unwrap(),
            semantics: MessageSemantics {
                squad: alice_member.squad_id,
                sender: alice_member.id,
                recipient: bob_member.id,
                body: MessageBody::new("durable").unwrap(),
                priority: MessagePriority::Normal,
                reply_to: None,
                correlation_id: None,
            },
            dedupe_key: DedupeKey::new("expiry-retry").unwrap(),
            created_at: millis(110),
        };
        store.authenticated_send(&active, &message).unwrap();
        let expired = AuthenticatedSession {
            instance_id: &instance.id,
            resume_token: &token,
            now: millis(130),
        };
        assert_eq!(
            store.authenticated_send(&expired, &message).unwrap().id,
            message.id
        );
        let mut changed = message.clone();
        changed.semantics.body = MessageBody::new("different").unwrap();
        assert!(matches!(
            store.authenticated_send(&expired, &changed),
            Err(RepositoryError::IdempotencyConflict)
        ));
        let mut new = message.clone();
        new.id = MessageId::new("msg_afterexpiry").unwrap();
        new.dedupe_key = DedupeKey::new("new-after-expiry").unwrap();
        assert!(matches!(
            store.authenticated_send(&expired, &new),
            Err(RepositoryError::LeaseExpired)
        ));
    }

    #[test]
    fn left_credential_can_repeat_exact_commit_but_cannot_change_or_send_new() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        let alice = store
            .join_and_claim(&join_request("alice", "alice", 100))
            .unwrap();
        let bob = store
            .join_and_claim(&join_request("bob", "bob", 101))
            .unwrap();
        let (alice_member, instance, token) = alice.into_parts();
        let (bob_member, _, _) = bob.into_parts();
        let session = AuthenticatedSession {
            instance_id: &instance.id,
            resume_token: &token,
            now: millis(110),
        };
        let message = SendMessage {
            id: MessageId::new("msg_leave").unwrap(),
            semantics: MessageSemantics {
                squad: alice_member.squad_id,
                sender: alice_member.id,
                recipient: bob_member.id,
                body: MessageBody::new("durable").unwrap(),
                priority: MessagePriority::Normal,
                reply_to: None,
                correlation_id: None,
            },
            dedupe_key: DedupeKey::new("leave-retry").unwrap(),
            created_at: millis(110),
        };
        store.authenticated_send(&session, &message).unwrap();
        store
            .authenticated_leave(&session, &SquadName::new("alpha").unwrap())
            .unwrap();
        assert_eq!(
            store.authenticated_send(&session, &message).unwrap().id,
            message.id
        );
        let mut changed = message.clone();
        changed.semantics.body = MessageBody::new("different").unwrap();
        assert!(matches!(
            store.authenticated_send(&session, &changed),
            Err(RepositoryError::IdempotencyConflict)
        ));
        let mut new = message.clone();
        new.id = MessageId::new("msg_newleave").unwrap();
        new.dedupe_key = DedupeKey::new("new-after-leave").unwrap();
        assert!(matches!(
            store.authenticated_send(&session, &new),
            Err(RepositoryError::NotMember)
        ));
    }

    #[test]
    fn invalid_bootstrap_metadata_writes_nothing() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        let mut request = join_request("alice", "alice", 100);
        request.client_kind = " ";
        assert!(matches!(
            store.join_and_claim(&request),
            Err(RepositoryError::InvalidRequest)
        ));
        for table in ["squads", "agents", "memberships", "instances"] {
            let count: i64 = store
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "invalid bootstrap wrote {table}");
        }
        for fault in [
            JoinClaimFault::Squad,
            JoinClaimFault::Agent,
            JoinClaimFault::Membership,
            JoinClaimFault::Instance,
        ] {
            let directory = TempDir::new().unwrap();
            let mut store = Store::open(directory.path().join("psst.db")).unwrap();
            let request = join_request("alice", "alice", 100);
            assert!(matches!(
                store.join_and_claim_inner(&request, Some(fault)),
                Err(RepositoryError::InjectedFailure)
            ));
            for table in ["squads", "agents", "memberships", "instances"] {
                let count: i64 = store
                    .connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(count, 0, "faulted bootstrap left {table}");
            }
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_protected_command_conceals_bad_credentials_and_prioritizes_expiry() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        let (alice, instance, token) = store
            .join_and_claim(&join_request("alice", "alice", 100))
            .unwrap()
            .into_parts();
        let (bob, _, _) = store
            .join_and_claim(&join_request("bob", "bob", 101))
            .unwrap()
            .into_parts();
        let send = message("matrix", &alice, &bob, 110);
        let wrong_token =
            ResumeToken::from_encoded("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let unknown_id = InstanceId::new("ins_unknown").unwrap();
        let wrong_token_session = AuthenticatedSession {
            instance_id: &instance.id,
            resume_token: &wrong_token,
            now: millis(110),
        };
        let wrong_id_session = AuthenticatedSession {
            instance_id: &unknown_id,
            resume_token: &token,
            now: millis(110),
        };
        let policy =
            LeasePolicy::new(Duration::from_millis(10), Duration::from_millis(30)).unwrap();
        let transcript = TranscriptQuery {
            squad: alice.squad_id.clone(),
            after: 0,
            limit: 10,
        };

        for session in [&wrong_token_session, &wrong_id_session] {
            assert!(matches!(
                store.authenticated_heartbeat(
                    session,
                    Availability::Unknown,
                    AvailabilitySource::Unknown,
                    policy
                ),
                Err(RepositoryError::NotFound)
            ));
            assert!(matches!(
                store.authenticated_send(session, &send),
                Err(RepositoryError::NotFound)
            ));
            assert!(matches!(
                store.authenticated_pending(session, 10),
                Err(RepositoryError::NotFound)
            ));
            assert!(matches!(
                store.authenticated_acknowledge(session, vec![send.id.clone()]),
                Err(RepositoryError::NotFound)
            ));
            assert!(matches!(
                store.authenticated_transcript(session, &transcript),
                Err(RepositoryError::NotFound)
            ));
            assert!(matches!(
                store.authenticated_leave(session, &SquadName::new("alpha").unwrap()),
                Err(RepositoryError::NotFound)
            ));
            assert!(matches!(
                store.authenticated_archive(session, &alice.squad_id),
                Err(RepositoryError::NotFound)
            ));
        }

        let active = AuthenticatedSession {
            instance_id: &instance.id,
            resume_token: &token,
            now: millis(110),
        };
        store.authenticated_send(&active, &send).unwrap();
        let expired = AuthenticatedSession {
            instance_id: &instance.id,
            resume_token: &token,
            now: millis(130),
        };
        assert!(matches!(
            store.authenticated_heartbeat(
                &expired,
                Availability::Unknown,
                AvailabilitySource::Unknown,
                policy
            ),
            Err(RepositoryError::LeaseExpired)
        ));
        assert_eq!(
            store.authenticated_send(&expired, &send).unwrap().id,
            send.id
        );
        let mut changed = send.clone();
        changed.semantics.body = MessageBody::new("different").unwrap();
        assert!(matches!(
            store.authenticated_send(&expired, &changed),
            Err(RepositoryError::IdempotencyConflict)
        ));
        let new_send = message("expired-new", &alice, &bob, 130);
        assert!(matches!(
            store.authenticated_send(&expired, &new_send),
            Err(RepositoryError::LeaseExpired)
        ));
        assert!(matches!(
            store.authenticated_pending(&expired, 10),
            Err(RepositoryError::LeaseExpired)
        ));
        assert!(matches!(
            store.authenticated_acknowledge(&expired, vec![send.id.clone()]),
            Err(RepositoryError::LeaseExpired)
        ));
        assert!(matches!(
            store.authenticated_transcript(&expired, &transcript),
            Err(RepositoryError::LeaseExpired)
        ));
        assert!(matches!(
            store.authenticated_leave(&expired, &SquadName::new("alpha").unwrap()),
            Err(RepositoryError::LeaseExpired)
        ));
        assert!(matches!(
            store.authenticated_archive(&expired, &alice.squad_id),
            Err(RepositoryError::LeaseExpired)
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn resume_conceals_credentials_and_obeys_live_expired_closed_and_left_ownership() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        let (member, instance, token) = store
            .join_and_claim(&join_request("alice", "alice", 100))
            .unwrap()
            .into_parts();
        let policy =
            LeasePolicy::new(Duration::from_millis(10), Duration::from_millis(30)).unwrap();
        let wrong =
            ResumeToken::from_encoded("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let resume = |id: &str| InstanceId::new(id).unwrap();
        assert!(matches!(
            store.authenticated_resume(
                &instance.id,
                &wrong,
                resume("ins_wrong"),
                AgentMode::Cooperative,
                "test",
                None,
                Availability::Unknown,
                AvailabilitySource::Unknown,
                millis(130),
                policy,
                &SquadName::new("alpha").unwrap()
            ),
            Err(RepositoryError::NotFound)
        ));
        assert!(matches!(
            store.authenticated_resume(
                &InstanceId::new("ins_missing").unwrap(),
                &token,
                resume("ins_missingnew"),
                AgentMode::Cooperative,
                "test",
                None,
                Availability::Unknown,
                AvailabilitySource::Unknown,
                millis(130),
                policy,
                &SquadName::new("alpha").unwrap()
            ),
            Err(RepositoryError::NotFound)
        ));
        assert!(matches!(
            store.authenticated_resume(
                &instance.id,
                &token,
                resume("ins_tooearly"),
                AgentMode::Cooperative,
                "test",
                None,
                Availability::Unknown,
                AvailabilitySource::Unknown,
                millis(120),
                policy,
                &SquadName::new("alpha").unwrap()
            ),
            Err(RepositoryError::NameInUse)
        ));

        let resumed = store
            .authenticated_resume(
                &instance.id,
                &token,
                resume("ins_resumed"),
                AgentMode::Cooperative,
                "test",
                None,
                Availability::Unknown,
                AvailabilitySource::Unknown,
                millis(130),
                policy,
                &SquadName::new("alpha").unwrap(),
            )
            .unwrap();
        assert_eq!(resumed.instance.membership_id, member.id);
        assert!(matches!(
            store.authenticated_resume(
                &instance.id,
                &token,
                resume("ins_steal"),
                AgentMode::Cooperative,
                "test",
                None,
                Availability::Unknown,
                AvailabilitySource::Unknown,
                millis(140),
                policy,
                &SquadName::new("alpha").unwrap()
            ),
            Err(RepositoryError::NameInUse)
        ));

        let resumed_session = AuthenticatedSession {
            instance_id: &resumed.instance.id,
            resume_token: &token,
            now: millis(140),
        };
        store
            .authenticated_leave(&resumed_session, &SquadName::new("alpha").unwrap())
            .unwrap();
        assert!(matches!(
            store.authenticated_resume(
                &resumed.instance.id,
                &token,
                resume("ins_afterleave"),
                AgentMode::Cooperative,
                "test",
                None,
                Availability::Unknown,
                AvailabilitySource::Unknown,
                millis(141),
                policy,
                &SquadName::new("alpha").unwrap()
            ),
            Err(RepositoryError::NotFound)
        ));
    }

    #[test]
    fn concurrent_send_and_archive_have_only_serializable_outcomes() {
        AuthenticatedRaceTests::concurrent_send_and_archive_have_only_serializable_outcomes();
    }

    #[test]
    fn concurrent_leave_and_resume_preserve_single_membership_owner() {
        AuthenticatedRaceTests::concurrent_leave_and_resume_preserve_single_membership_owner();
    }

    #[test]
    fn concurrent_resume_and_send_never_send_as_replacement_owner() {
        AuthenticatedRaceTests::concurrent_resume_and_send_never_send_as_replacement_owner();
    }
}
