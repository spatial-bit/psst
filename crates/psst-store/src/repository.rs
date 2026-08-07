use psst_core::{
    AgentId, AgentMode, Availability, AvailabilityObservation, AvailabilitySource, ErrorCode,
    MemberName, MembershipId, Mission, Role, SquadId, SquadName, SquadState, UnixMillis,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::Store;

/// A request to create a durable squad.
#[derive(Clone, Debug)]
pub struct CreateSquad {
    pub id: SquadId,
    pub name: SquadName,
    pub mission: Mission,
    pub created_at: UnixMillis,
}

/// A request to create a new agent identity and membership.
#[derive(Clone, Debug)]
pub struct JoinMembership {
    pub squad_name: SquadName,
    pub mission_if_missing: Option<Mission>,
    pub squad_id_if_missing: SquadId,
    pub agent_id: AgentId,
    pub membership_id: MembershipId,
    pub member_name: MemberName,
    pub role: Role,
    pub joined_at: UnixMillis,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SquadRecord {
    pub id: SquadId,
    pub name: SquadName,
    pub mission: Mission,
    pub state: SquadState,
    pub created_at: UnixMillis,
    pub archived_at: Option<UnixMillis>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MembershipRecord {
    pub id: MembershipId,
    pub squad_id: SquadId,
    pub agent_id: AgentId,
    pub name: MemberName,
    pub role: Role,
    pub joined_at: UnixMillis,
    pub left_at: Option<UnixMillis>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportPresence {
    Online,
    Offline,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RosterMember {
    pub membership: MembershipRecord,
    pub presence: TransportPresence,
    pub mode: Option<AgentMode>,
    pub availability: AvailabilityObservation,
    pub last_seen_at: Option<UnixMillis>,
}

/// Stable, non-sensitive repository errors suitable for an interface adapter.
#[derive(Debug)]
pub enum RepositoryError {
    InvalidRequest,
    NotFound,
    SquadArchived,
    NotMember,
    NameInUse,
    LeaseExpired,
    RecipientNotFound,
    IdempotencyConflict,
    PayloadTooLarge,
    DatabaseBusy,
    Internal(rusqlite::Error),
    InvalidStoredData,
    EntropyUnavailable,
    InjectedFailure,
}

impl RepositoryError {
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::InvalidRequest => ErrorCode::InvalidRequest,
            Self::NotFound => ErrorCode::NotFound,
            Self::SquadArchived => ErrorCode::SquadArchived,
            Self::NotMember => ErrorCode::NotMember,
            Self::NameInUse => ErrorCode::NameInUse,
            Self::LeaseExpired => ErrorCode::LeaseExpired,
            Self::RecipientNotFound => ErrorCode::RecipientNotFound,
            Self::IdempotencyConflict => ErrorCode::IdempotencyConflict,
            Self::PayloadTooLarge => ErrorCode::PayloadTooLarge,
            Self::DatabaseBusy => ErrorCode::DatabaseBusy,
            Self::Internal(_)
            | Self::InvalidStoredData
            | Self::EntropyUnavailable
            | Self::InjectedFailure => ErrorCode::InternalError,
        }
    }
}

impl std::fmt::Display for RepositoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRequest => "the store request is invalid",
            Self::NotFound => "the requested resource was not found",
            Self::SquadArchived => "the squad is archived",
            Self::NotMember => "the membership is not active",
            Self::NameInUse => "the requested membership name is in use",
            Self::LeaseExpired => "the instance lease has expired",
            Self::RecipientNotFound => "the recipient membership was not found",
            Self::IdempotencyConflict => "the dedupe key has different message semantics",
            Self::PayloadTooLarge => "the message payload is too large",
            Self::DatabaseBusy => "the database is busy",
            Self::Internal(_)
            | Self::InvalidStoredData
            | Self::EntropyUnavailable
            | Self::InjectedFailure => "the store operation failed",
        })
    }
}

impl std::error::Error for RepositoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Internal(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for RepositoryError {
    fn from(error: rusqlite::Error) -> Self {
        if super::is_busy(&error) {
            Self::DatabaseBusy
        } else {
            Self::Internal(error)
        }
    }
}

impl Store {
    /// Creates one active squad.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError::NameInUse`] for a duplicate squad name, or a
    /// stable storage error if the transaction cannot commit.
    pub fn create_squad(&mut self, request: &CreateSquad) -> Result<SquadRecord, RepositoryError> {
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "INSERT INTO squads(id, name, mission, state, created_at)
             VALUES (?1, ?2, ?3, 'active', ?4)
             ON CONFLICT(name) DO NOTHING",
            params![
                request.id.as_str(),
                request.name.as_str(),
                request.mission.as_str(),
                request.created_at.as_i64()
            ],
        )?;
        if changed == 0 {
            return Err(RepositoryError::NameInUse);
        }
        transaction.commit()?;
        Ok(SquadRecord {
            id: request.id.clone(),
            name: request.name.clone(),
            mission: request.mission.clone(),
            state: SquadState::Active,
            created_at: request.created_at,
            archived_at: None,
        })
    }

    /// Lists every squad, including archived squads, in routing-name order.
    ///
    /// # Errors
    ///
    /// Returns a stable storage error if the query fails or stored data is invalid.
    pub fn list_squads(&self) -> Result<Vec<SquadRecord>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT id, name, mission, state, created_at, archived_at
             FROM squads ORDER BY name",
        )?;
        statement
            .query_map([], map_squad)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(RepositoryError::from)
    }

    /// Describes an active or archived squad.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError::NotFound`] when the name is unknown, or a stable
    /// storage error if the query fails or stored data is invalid.
    pub fn describe_squad(&self, name: &SquadName) -> Result<SquadRecord, RepositoryError> {
        self.connection
            .query_row(
                "SELECT id, name, mission, state, created_at, archived_at
                 FROM squads WHERE name = ?1",
                [name.as_str()],
                map_squad,
            )
            .optional()?
            .ok_or(RepositoryError::NotFound)
    }

    /// Irreversibly archives an active squad.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError::NotFound`] for an unknown squad,
    /// [`RepositoryError::SquadArchived`] if already archived, or a storage error.
    pub fn archive_squad(
        &mut self,
        name: &SquadName,
        archived_at: UnixMillis,
    ) -> Result<(), RepositoryError> {
        let changed = self.connection.execute(
            "UPDATE squads SET state = 'archived', archived_at = ?2
             WHERE name = ?1 AND state = 'active'",
            params![name.as_str(), archived_at.as_i64()],
        )?;
        if changed == 1 {
            return Ok(());
        }
        match self.describe_squad(name) {
            Ok(_) => Err(RepositoryError::SquadArchived),
            Err(RepositoryError::NotFound) => Err(RepositoryError::NotFound),
            Err(error) => Err(error),
        }
    }

    /// Atomically creates an agent and membership, optionally creating its squad.
    ///
    /// # Errors
    ///
    /// Returns stable lifecycle or uniqueness errors, or a storage error when the
    /// transaction cannot commit. No partial identity rows remain on error.
    pub fn join(&mut self, request: &JoinMembership) -> Result<MembershipRecord, RepositoryError> {
        self.join_with_fault(request, None)
    }

    fn join_with_fault(
        &mut self,
        request: &JoinMembership,
        fault: Option<JoinFault>,
    ) -> Result<MembershipRecord, RepositoryError> {
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let squad_id = find_or_create_squad(&transaction, request)?;
        inject(fault, JoinFault::Squad)?;
        transaction.execute(
            "INSERT INTO agents(id, created_at) VALUES (?1, ?2)",
            params![request.agent_id.as_str(), request.joined_at.as_i64()],
        )?;
        inject(fault, JoinFault::Agent)?;
        let changed = transaction.execute(
            "INSERT INTO memberships(
                 id, squad_id, agent_id, name, normalized_name, role, joined_at
             ) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)
             ON CONFLICT(squad_id, normalized_name) WHERE left_at IS NULL DO NOTHING",
            params![
                request.membership_id.as_str(),
                squad_id.as_str(),
                request.agent_id.as_str(),
                request.member_name.as_str(),
                request.role.as_str(),
                request.joined_at.as_i64()
            ],
        )?;
        if changed == 0 {
            return Err(RepositoryError::NameInUse);
        }
        inject(fault, JoinFault::Membership)?;
        transaction.commit()?;
        Ok(MembershipRecord {
            id: request.membership_id.clone(),
            squad_id,
            agent_id: request.agent_id.clone(),
            name: request.member_name.clone(),
            role: request.role.clone(),
            joined_at: request.joined_at,
            left_at: None,
        })
    }

    /// Reads durable membership history and current lease-derived presence.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError::NotFound`] for an unknown squad, or a stable
    /// storage error if the query fails or stored data is invalid.
    pub fn roster(
        &self,
        squad_name: &SquadName,
        now: UnixMillis,
    ) -> Result<Vec<RosterMember>, RepositoryError> {
        let squad = self.describe_squad(squad_name)?;
        let mut statement = self.connection.prepare(
            "SELECT m.id, m.agent_id, m.name, m.role, m.joined_at, m.left_at,
                    i.mode, i.lease_expires_at, i.last_seen_at, i.closed_at,
                    i.availability, i.availability_source, i.availability_observed_at
             FROM memberships m
             LEFT JOIN instances i ON i.id = (
                 SELECT candidate.id FROM instances candidate
                 WHERE candidate.membership_id = m.id
                 ORDER BY candidate.created_at DESC, candidate.id DESC LIMIT 1
             )
             WHERE m.squad_id = ?1
             ORDER BY m.joined_at, m.id",
        )?;
        let rows = statement.query_map([squad.id.as_str()], |row| {
            let membership = MembershipRecord {
                id: parse(&row.get::<_, String>(0)?)?,
                squad_id: squad.id.clone(),
                agent_id: parse(&row.get::<_, String>(1)?)?,
                name: parse(&row.get::<_, String>(2)?)?,
                role: parse(&row.get::<_, String>(3)?)?,
                joined_at: timestamp(row.get(4)?)?,
                left_at: optional_timestamp(row.get(5)?)?,
            };
            let mode = parse_mode(row.get::<_, Option<String>>(6)?.as_deref())?;
            let lease_expiry: Option<i64> = row.get(7)?;
            let last_seen_at = optional_timestamp(row.get(8)?)?;
            let closed_at: Option<i64> = row.get(9)?;
            let online = membership.left_at.is_none()
                && closed_at.is_none()
                && lease_expiry.is_some_and(|expiry| expiry > now.as_i64());
            let availability = if online {
                AvailabilityObservation::new(
                    parse_availability(row.get::<_, Option<String>>(10)?.as_deref())?,
                    parse_availability_source(row.get::<_, Option<String>>(11)?.as_deref())?,
                    timestamp(row.get(12)?)?,
                )
                .map_err(|_| invalid_data())?
            } else {
                AvailabilityObservation::new(
                    Availability::Unknown,
                    AvailabilitySource::Unknown,
                    now,
                )
                .map_err(|_| invalid_data())?
            };
            Ok(RosterMember {
                membership,
                presence: if online {
                    TransportPresence::Online
                } else {
                    TransportPresence::Offline
                },
                mode,
                availability,
                last_seen_at,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(RepositoryError::from)
    }

    /// Leaves a membership and closes all of its unclosed instances atomically.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError::NotMember`] when the immutable membership is not
    /// active in the specified squad,
    /// or a stable storage error when the transaction cannot commit.
    pub fn leave(
        &mut self,
        squad_id: &SquadId,
        membership_id: &MembershipId,
        left_at: UnixMillis,
    ) -> Result<(), RepositoryError> {
        self.leave_with_fault(squad_id, membership_id, left_at, false)
    }

    fn leave_with_fault(
        &mut self,
        squad_id: &SquadId,
        membership_id: &MembershipId,
        left_at: UnixMillis,
        fail_after_instance_close: bool,
    ) -> Result<(), RepositoryError> {
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM memberships
                 WHERE id = ?1 AND squad_id = ?2 AND left_at IS NULL)",
            params![membership_id.as_str(), squad_id.as_str()],
            |row| row.get(0),
        )?;
        if !active {
            return Err(RepositoryError::NotMember);
        }
        transaction.execute(
            "UPDATE instances SET closed_at = ?2
             WHERE membership_id = ?1 AND closed_at IS NULL",
            params![membership_id.as_str(), left_at.as_i64()],
        )?;
        if fail_after_instance_close {
            return Err(RepositoryError::InjectedFailure);
        }
        transaction.execute(
            "UPDATE memberships SET left_at = ?2 WHERE id = ?1 AND left_at IS NULL",
            params![membership_id.as_str(), left_at.as_i64()],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

fn find_or_create_squad(
    transaction: &Transaction<'_>,
    request: &JoinMembership,
) -> Result<SquadId, RepositoryError> {
    let existing: Option<(String, String)> = transaction
        .query_row(
            "SELECT id, state FROM squads WHERE name = ?1",
            [request.squad_name.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((id, state)) = existing {
        if state == "archived" {
            return Err(RepositoryError::SquadArchived);
        }
        return Ok(parse(&id)?);
    }
    let mission = request
        .mission_if_missing
        .as_ref()
        .ok_or(RepositoryError::NotFound)?;
    transaction.execute(
        "INSERT INTO squads(id, name, mission, state, created_at)
         VALUES (?1, ?2, ?3, 'active', ?4)",
        params![
            request.squad_id_if_missing.as_str(),
            request.squad_name.as_str(),
            mission.as_str(),
            request.joined_at.as_i64()
        ],
    )?;
    Ok(request.squad_id_if_missing.clone())
}

fn map_squad(row: &rusqlite::Row<'_>) -> rusqlite::Result<SquadRecord> {
    Ok(SquadRecord {
        id: parse(&row.get::<_, String>(0)?)?,
        name: parse(&row.get::<_, String>(1)?)?,
        mission: parse(&row.get::<_, String>(2)?)?,
        state: match row.get::<_, String>(3)?.as_str() {
            "active" => SquadState::Active,
            "archived" => SquadState::Archived,
            _ => return Err(invalid_data()),
        },
        created_at: timestamp(row.get(4)?)?,
        archived_at: optional_timestamp(row.get(5)?)?,
    })
}

fn parse<T: std::str::FromStr>(value: &str) -> rusqlite::Result<T> {
    value.parse().map_err(|_| invalid_data())
}

fn timestamp(value: i64) -> rusqlite::Result<UnixMillis> {
    UnixMillis::new(value).map_err(|_| invalid_data())
}

fn optional_timestamp(value: Option<i64>) -> rusqlite::Result<Option<UnixMillis>> {
    value.map(timestamp).transpose()
}

fn parse_availability(value: Option<&str>) -> rusqlite::Result<Availability> {
    match value {
        Some("idle") => Ok(Availability::Idle),
        Some("busy") => Ok(Availability::Busy),
        Some("blocked") => Ok(Availability::Blocked),
        Some("unknown") | None => Ok(Availability::Unknown),
        Some(_) => Err(invalid_data()),
    }
}

fn parse_availability_source(value: Option<&str>) -> rusqlite::Result<AvailabilitySource> {
    match value {
        Some("session_lifecycle") => Ok(AvailabilitySource::SessionLifecycle),
        Some("mcp_connection") => Ok(AvailabilitySource::McpConnection),
        Some("tool_activity") => Ok(AvailabilitySource::ToolActivity),
        Some("agent_reported") => Ok(AvailabilitySource::AgentReported),
        Some("unknown") | None => Ok(AvailabilitySource::Unknown),
        Some(_) => Err(invalid_data()),
    }
}

fn parse_mode(value: Option<&str>) -> rusqlite::Result<Option<AgentMode>> {
    match value {
        Some("cooperative") => Ok(Some(AgentMode::Cooperative)),
        Some("scheduled") => Ok(Some(AgentMode::Scheduled)),
        Some("harnessed") => Ok(Some(AgentMode::Harnessed)),
        None => Ok(None),
        Some(_) => Err(invalid_data()),
    }
}

fn invalid_data() -> rusqlite::Error {
    rusqlite::Error::InvalidQuery
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum JoinFault {
    Squad,
    Agent,
    Membership,
}

fn inject(actual: Option<JoinFault>, point: JoinFault) -> Result<(), RepositoryError> {
    if actual == Some(point) {
        Err(RepositoryError::InjectedFailure)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use tempfile::TempDir;

    use super::*;

    fn squad(name: &str, id: &str) -> CreateSquad {
        CreateSquad {
            id: SquadId::new(id).unwrap(),
            name: SquadName::new(name).unwrap(),
            mission: Mission::new("Build reliable software").unwrap(),
            created_at: UnixMillis::new(100).unwrap(),
        }
    }

    fn join(name: &str, suffix: &str) -> JoinMembership {
        JoinMembership {
            squad_name: SquadName::new("alpha").unwrap(),
            mission_if_missing: None,
            squad_id_if_missing: SquadId::new("sqd_alpha").unwrap(),
            agent_id: AgentId::new(format!("agt_{suffix}")).unwrap(),
            membership_id: MembershipId::new(format!("mem_{suffix}")).unwrap(),
            member_name: MemberName::new(name).unwrap(),
            role: Role::new("engineer").unwrap(),
            joined_at: UnixMillis::new(200).unwrap(),
        }
    }

    #[test]
    fn squad_lifecycle_is_durable_and_archive_is_irreversible() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let mut store = Store::open(&path).unwrap();
        let created = store.create_squad(&squad("alpha", "sqd_alpha")).unwrap();
        assert_eq!(store.list_squads().unwrap(), vec![created.clone()]);
        assert_eq!(store.describe_squad(&created.name).unwrap(), created);
        store
            .archive_squad(
                &SquadName::new("alpha").unwrap(),
                UnixMillis::new(300).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            store.archive_squad(
                &SquadName::new("alpha").unwrap(),
                UnixMillis::new(400).unwrap()
            ),
            Err(RepositoryError::SquadArchived)
        ));
        drop(store);
        let store = Store::open(path).unwrap();
        let archived = store
            .describe_squad(&SquadName::new("alpha").unwrap())
            .unwrap();
        assert_eq!(archived.state, SquadState::Archived);
        assert_eq!(archived.mission.as_str(), "Build reliable software");
    }

    #[test]
    fn duplicate_squad_name_has_stable_error() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        store.create_squad(&squad("alpha", "sqd_alpha")).unwrap();
        let error = store
            .create_squad(&squad("alpha", "sqd_other"))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::NameInUse);
        assert!(!error.to_string().to_lowercase().contains("sqlite"));
    }

    #[test]
    fn implicit_create_requires_mission_and_does_not_replace_existing_mission() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        let mut request = join("alice", "alice");
        assert!(matches!(
            store.join(&request),
            Err(RepositoryError::NotFound)
        ));
        request.mission_if_missing = Some(Mission::new("Original mission").unwrap());
        store.join(&request).unwrap();
        let mut second = join("bob", "bob");
        second.mission_if_missing = Some(Mission::new("Replacement mission").unwrap());
        store.join(&second).unwrap();
        assert_eq!(
            store
                .describe_squad(&request.squad_name)
                .unwrap()
                .mission
                .as_str(),
            "Original mission"
        );
    }

    #[test]
    fn independent_connections_allow_exactly_one_simultaneous_name_claim() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        Store::open(&path)
            .unwrap()
            .create_squad(&squad("alpha", "sqd_alpha"))
            .unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = ["one", "two"]
            .into_iter()
            .map(|suffix| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut store = Store::open(path).unwrap();
                    let request = join("alice", suffix);
                    barrier.wait();
                    store.join(&request)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(
                    |result| matches!(result, Err(error) if error.code() == ErrorCode::NameInUse)
                )
                .count(),
            1
        );
    }

    #[test]
    fn names_are_scoped_to_squad_and_reusable_only_after_leave() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        store.create_squad(&squad("alpha", "sqd_alpha")).unwrap();
        store.create_squad(&squad("beta", "sqd_beta")).unwrap();
        store.join(&join("alice", "first")).unwrap();
        let mut other = join("alice", "other");
        other.squad_name = SquadName::new("beta").unwrap();
        store.join(&other).unwrap();
        store
            .leave(
                &SquadId::new("sqd_alpha").unwrap(),
                &MembershipId::new("mem_first").unwrap(),
                UnixMillis::new(300).unwrap(),
            )
            .unwrap();
        store.join(&join("alice", "replacement")).unwrap();
        let roster = store
            .roster(
                &SquadName::new("alpha").unwrap(),
                UnixMillis::new(400).unwrap(),
            )
            .unwrap();
        assert_eq!(roster.len(), 2);
        assert!(roster[0].membership.left_at.is_some());
        assert!(roster[1].membership.left_at.is_none());
    }

    #[test]
    fn stale_former_owner_cannot_leave_name_replacement() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        store.create_squad(&squad("alpha", "sqd_alpha")).unwrap();
        let former = store.join(&join("alice", "former")).unwrap();
        store
            .leave(&former.squad_id, &former.id, UnixMillis::new(300).unwrap())
            .unwrap();
        let replacement = store.join(&join("alice", "replacement")).unwrap();

        assert!(matches!(
            store.leave(&former.squad_id, &former.id, UnixMillis::new(400).unwrap()),
            Err(RepositoryError::NotMember)
        ));
        let roster = store
            .roster(
                &SquadName::new("alpha").unwrap(),
                UnixMillis::new(400).unwrap(),
            )
            .unwrap();
        assert!(roster.iter().any(
            |entry| entry.membership.id == replacement.id && entry.membership.left_at.is_none()
        ));
    }

    #[test]
    fn archived_squad_rejects_join_and_preserves_roster() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        store.create_squad(&squad("alpha", "sqd_alpha")).unwrap();
        store.join(&join("alice", "alice")).unwrap();
        store
            .archive_squad(
                &SquadName::new("alpha").unwrap(),
                UnixMillis::new(300).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            store.join(&join("bob", "bob")),
            Err(RepositoryError::SquadArchived)
        ));
        assert_eq!(
            store
                .roster(
                    &SquadName::new("alpha").unwrap(),
                    UnixMillis::new(400).unwrap()
                )
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn roster_reports_expired_or_absent_instances_as_offline_unknown() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        store.create_squad(&squad("alpha", "sqd_alpha")).unwrap();
        let member = store.join(&join("alice", "alice")).unwrap();
        let absent = store
            .roster(
                &SquadName::new("alpha").unwrap(),
                UnixMillis::new(150).unwrap(),
            )
            .unwrap();
        assert_eq!(absent[0].presence, TransportPresence::Offline);
        assert_eq!(absent[0].mode, None);
        assert_eq!(absent[0].availability.availability(), Availability::Unknown);
        assert_eq!(absent[0].availability.source(), AvailabilitySource::Unknown);
        assert_eq!(absent[0].availability.observed_at().as_i64(), 150);
        store
            .connection
            .execute(
                "INSERT INTO instances(
                    id, membership_id, mode, client_kind, resume_token_hash, availability,
                    availability_source, availability_observed_at, lease_expires_at,
                    last_seen_at, created_at
                 ) VALUES ('ins_expired', ?1, 'cooperative', 'test', zeroblob(32), 'idle',
                    'tool_activity', 100, 200, 190, 100)",
                [member.id.as_str()],
            )
            .unwrap();
        let roster = store
            .roster(
                &SquadName::new("alpha").unwrap(),
                UnixMillis::new(199).unwrap(),
            )
            .unwrap();
        assert_eq!(roster[0].presence, TransportPresence::Online);
        assert_eq!(roster[0].mode, Some(AgentMode::Cooperative));
        assert_eq!(roster[0].availability.availability(), Availability::Idle);
        assert_eq!(
            roster[0].availability.source(),
            AvailabilitySource::ToolActivity
        );
        assert_eq!(roster[0].availability.observed_at().as_i64(), 100);
        let roster = store
            .roster(
                &SquadName::new("alpha").unwrap(),
                UnixMillis::new(200).unwrap(),
            )
            .unwrap();
        assert_eq!(roster[0].presence, TransportPresence::Offline);
        assert_eq!(roster[0].mode, Some(AgentMode::Cooperative));
        assert_eq!(roster[0].availability.availability(), Availability::Unknown);
        assert_eq!(roster[0].availability.source(), AvailabilitySource::Unknown);
        assert_eq!(roster[0].availability.observed_at().as_i64(), 200);
    }

    #[test]
    fn leave_closes_unclosed_instance_in_same_transaction() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        store.create_squad(&squad("alpha", "sqd_alpha")).unwrap();
        let member = store.join(&join("alice", "alice")).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO instances(
                    id, membership_id, mode, client_kind, resume_token_hash, availability,
                    availability_source, availability_observed_at, lease_expires_at,
                    last_seen_at, created_at
                 ) VALUES ('ins_live', ?1, 'cooperative', 'test', zeroblob(32), 'busy',
                    'tool_activity', 100, 1000, 100, 100)",
                [member.id.as_str()],
            )
            .unwrap();
        store
            .leave(
                &SquadId::new("sqd_alpha").unwrap(),
                &member.id,
                UnixMillis::new(300).unwrap(),
            )
            .unwrap();
        let (left_at, closed_at): (i64, i64) = store
            .connection
            .query_row(
                "SELECT m.left_at, i.closed_at FROM memberships m
                 JOIN instances i ON i.membership_id = m.id WHERE m.id = ?1",
                [member.id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((left_at, closed_at), (300, 300));
    }

    #[test]
    fn failed_leave_rolls_back_instance_and_membership_changes() {
        let directory = TempDir::new().unwrap();
        let mut store = Store::open(directory.path().join("psst.db")).unwrap();
        store.create_squad(&squad("alpha", "sqd_alpha")).unwrap();
        let member = store.join(&join("alice", "alice")).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO instances(
                    id, membership_id, mode, client_kind, resume_token_hash, availability,
                    availability_source, availability_observed_at, lease_expires_at,
                    last_seen_at, created_at
                 ) VALUES ('ins_live', ?1, 'cooperative', 'test', zeroblob(32), 'busy',
                    'tool_activity', 100, 1000, 100, 100)",
                [member.id.as_str()],
            )
            .unwrap();
        assert!(matches!(
            store.leave_with_fault(
                &SquadId::new("sqd_alpha").unwrap(),
                &member.id,
                UnixMillis::new(300).unwrap(),
                true
            ),
            Err(RepositoryError::InjectedFailure)
        ));
        let (left_at, closed_at): (Option<i64>, Option<i64>) = store
            .connection
            .query_row(
                "SELECT m.left_at, i.closed_at FROM memberships m
                 JOIN instances i ON i.membership_id = m.id WHERE m.id = ?1",
                [member.id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((left_at, closed_at), (None, None));
    }

    #[test]
    fn membership_join_and_leave_survive_restarts() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let mut store = Store::open(&path).unwrap();
        store.create_squad(&squad("alpha", "sqd_alpha")).unwrap();
        let member = store.join(&join("alice", "persistent")).unwrap();
        drop(store);

        let mut store = Store::open(&path).unwrap();
        let roster = store
            .roster(
                &SquadName::new("alpha").unwrap(),
                UnixMillis::new(250).unwrap(),
            )
            .unwrap();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].membership, member);
        store
            .leave(
                &SquadId::new("sqd_alpha").unwrap(),
                &member.id,
                UnixMillis::new(300).unwrap(),
            )
            .unwrap();
        drop(store);

        let store = Store::open(path).unwrap();
        let roster = store
            .roster(
                &SquadName::new("alpha").unwrap(),
                UnixMillis::new(400).unwrap(),
            )
            .unwrap();
        assert_eq!(roster[0].membership.left_at.unwrap().as_i64(), 300);
    }

    #[test]
    fn archive_and_join_race_has_a_serializable_outcome() {
        for round in 0..8 {
            let directory = TempDir::new().unwrap();
            let path = directory.path().join("psst.db");
            Store::open(&path)
                .unwrap()
                .create_squad(&squad("alpha", "sqd_alpha"))
                .unwrap();
            let barrier = Arc::new(Barrier::new(2));
            let archive_path = path.clone();
            let archive_barrier = Arc::clone(&barrier);
            let archive = std::thread::spawn(move || {
                let mut store = Store::open(archive_path).unwrap();
                archive_barrier.wait();
                store.archive_squad(
                    &SquadName::new("alpha").unwrap(),
                    UnixMillis::new(300).unwrap(),
                )
            });
            let join_path = path.clone();
            let join_barrier = Arc::clone(&barrier);
            let claimant = std::thread::spawn(move || {
                let mut store = Store::open(join_path).unwrap();
                join_barrier.wait();
                store.join(&join("alice", "claimant"))
            });
            archive.join().unwrap().unwrap();
            let joined = claimant.join().unwrap();
            assert!(
                joined.is_ok() || matches!(&joined, Err(RepositoryError::SquadArchived)),
                "round {round} had a non-serializable result"
            );
            let store = Store::open(path).unwrap();
            assert_eq!(
                store
                    .describe_squad(&SquadName::new("alpha").unwrap())
                    .unwrap()
                    .state,
                SquadState::Archived
            );
            assert_eq!(
                store
                    .roster(
                        &SquadName::new("alpha").unwrap(),
                        UnixMillis::new(400).unwrap()
                    )
                    .unwrap()
                    .len(),
                usize::from(joined.is_ok())
            );
        }
    }

    #[test]
    fn stable_error_codes_and_display_never_expose_sql_details() {
        let internal = RepositoryError::Internal(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some("SELECT secret FROM private_table".to_owned()),
        ));
        let busy: RepositoryError = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database path and SQL details".to_owned()),
        )
        .into();
        for (error, code, display) in [
            (
                RepositoryError::SquadArchived,
                ErrorCode::SquadArchived,
                "the squad is archived",
            ),
            (
                RepositoryError::NotMember,
                ErrorCode::NotMember,
                "the membership is not active",
            ),
            (busy, ErrorCode::DatabaseBusy, "the database is busy"),
            (
                internal,
                ErrorCode::InternalError,
                "the store operation failed",
            ),
        ] {
            assert_eq!(error.code(), code);
            assert_eq!(error.to_string(), display);
            assert!(!error.to_string().contains("SELECT"));
        }
    }

    #[test]
    fn injected_failures_rollback_every_join_boundary() {
        for fault in [JoinFault::Squad, JoinFault::Agent, JoinFault::Membership] {
            let directory = TempDir::new().unwrap();
            let mut store = Store::open(directory.path().join("psst.db")).unwrap();
            let mut request = join("alice", "alice");
            request.mission_if_missing = Some(Mission::new("Mission").unwrap());
            assert!(matches!(
                store.join_with_fault(&request, Some(fault)),
                Err(RepositoryError::InjectedFailure)
            ));
            for table in ["squads", "agents", "memberships"] {
                let sql = format!("SELECT COUNT(*) FROM {table}");
                let count: i64 = store
                    .connection
                    .query_row(&sql, [], |row| row.get(0))
                    .unwrap();
                assert_eq!(count, 0, "partial row remained in {table}");
            }
        }
    }
}
