use std::time::Duration;

use psst_core::{
    AgentMode, Availability, AvailabilityObservation, AvailabilitySource, InstanceId,
    InstanceState, MembershipId, ResumeToken, UnixMillis, renew_lease,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{RepositoryError, Store};

pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
pub const DEFAULT_LEASE_DURATION: Duration = Duration::from_secs(30);
const TOKEN_HASH_DOMAIN: &[u8] = b"psst/resume-token/v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeasePolicy {
    heartbeat_interval: Duration,
    lease_duration: Duration,
}

impl LeasePolicy {
    /// Creates advertised lease timings. Both values must be positive and the lease
    /// must be longer than the heartbeat interval.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError::InvalidRequest`] for invalid or unrepresentable timings.
    pub fn new(
        heartbeat_interval: Duration,
        lease_duration: Duration,
    ) -> Result<Self, RepositoryError> {
        if heartbeat_interval.is_zero()
            || lease_duration <= heartbeat_interval
            || i64::try_from(lease_duration.as_millis()).is_err()
        {
            return Err(RepositoryError::InvalidRequest);
        }
        Ok(Self {
            heartbeat_interval,
            lease_duration,
        })
    }

    #[must_use]
    pub const fn heartbeat_interval(self) -> Duration {
        self.heartbeat_interval
    }

    #[must_use]
    pub const fn lease_duration(self) -> Duration {
        self.lease_duration
    }
}

impl Default for LeasePolicy {
    fn default() -> Self {
        Self {
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            lease_duration: DEFAULT_LEASE_DURATION,
        }
    }
}

/// Initial instance ownership request. Secret material is adapter-owned and is
/// accepted only through this non-serializable store boundary.
pub struct ClaimInstance<'a> {
    pub id: InstanceId,
    pub membership_id: MembershipId,
    pub mode: AgentMode,
    pub client_kind: &'a str,
    pub hostname: Option<&'a str>,
    pub availability: Availability,
    pub availability_source: AvailabilitySource,
    pub now: UnixMillis,
    pub lease_policy: LeasePolicy,
}

/// Adapter-only result of an initial claim. Deliberately neither `Debug` nor
/// serializable so generic responses and diagnostics cannot expose its secret.
pub struct ClaimOutcome {
    instance: InstanceRecord,
    resume_token: ResumeToken,
}

impl ClaimOutcome {
    #[must_use]
    pub const fn instance(&self) -> &InstanceRecord {
        &self.instance
    }

    /// Deliberate adapter credential access for secure local persistence.
    #[must_use]
    pub const fn resume_token(&self) -> &ResumeToken {
        &self.resume_token
    }

    /// Separates model-safe instance metadata from adapter-owned secret material.
    #[must_use]
    pub fn into_parts(self) -> (InstanceRecord, ResumeToken) {
        (self.instance, self.resume_token)
    }
}

pub struct ResumeInstance<'a> {
    pub id: InstanceId,
    pub membership_id: MembershipId,
    pub mode: AgentMode,
    pub client_kind: &'a str,
    pub hostname: Option<&'a str>,
    pub resume_token: &'a ResumeToken,
    pub availability: Availability,
    pub availability_source: AvailabilitySource,
    pub now: UnixMillis,
    pub lease_policy: LeasePolicy,
}

pub struct HeartbeatInstance<'a> {
    pub id: &'a InstanceId,
    pub resume_token: &'a ResumeToken,
    pub availability: Availability,
    pub availability_source: AvailabilitySource,
    pub now: UnixMillis,
    pub lease_policy: LeasePolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceRecord {
    pub id: InstanceId,
    pub membership_id: MembershipId,
    pub mode: AgentMode,
    pub availability: Availability,
    pub availability_source: AvailabilitySource,
    pub availability_observed_at: UnixMillis,
    pub lease_expires_at: UnixMillis,
    pub last_seen_at: UnixMillis,
    pub created_at: UnixMillis,
    pub closed_at: Option<UnixMillis>,
    pub heartbeat_interval: Duration,
    pub lease_duration: Duration,
}

impl Store {
    /// Claims the first instance for an active membership atomically.
    ///
    /// # Errors
    ///
    /// Returns a stable membership, ownership, validation, or storage error.
    pub fn claim_instance(
        &mut self,
        request: &ClaimInstance<'_>,
    ) -> Result<ClaimOutcome, RepositoryError> {
        validate_observation(
            request.availability,
            request.availability_source,
            request.now,
        )?;
        validate_client(request.client_kind, request.hostname)?;
        let resume_token =
            ResumeToken::generate().map_err(|_| RepositoryError::EntropyUnavailable)?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_membership(&transaction, &request.membership_id)?;
        let has_history: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM instances WHERE membership_id = ?1)",
            [request.membership_id.as_str()],
            |row| row.get(0),
        )?;
        if has_history {
            return Err(RepositoryError::NameInUse);
        }
        let record = insert_instance(
            &transaction,
            request.id.clone(),
            request.membership_id.clone(),
            request.mode,
            request.client_kind,
            request.hostname,
            &resume_token,
            request.availability,
            request.availability_source,
            request.now,
            request.lease_policy,
        )?;
        transaction.commit()?;
        Ok(ClaimOutcome {
            instance: record,
            resume_token,
        })
    }

    /// Renews only the token-authenticated current instance. Stored observation
    /// time is monotonic: a backward wall-clock reading neither rewinds state nor
    /// repeatedly extends the lease.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError::LeaseExpired`] at the expiry boundary, hides
    /// authentication failures as [`RepositoryError::NotMember`], or returns a stable storage error.
    pub fn heartbeat_instance(
        &mut self,
        request: &HeartbeatInstance<'_>,
    ) -> Result<InstanceRecord, RepositoryError> {
        validate_observation(
            request.availability,
            request.availability_source,
            request.now,
        )?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored: Option<(String, String, Vec<u8>, i64, i64, i64)> = transaction
            .query_row(
                "SELECT i.membership_id, i.mode, i.resume_token_hash,
                        i.lease_expires_at, i.last_seen_at, i.created_at
                 FROM instances i JOIN memberships m ON m.id = i.membership_id
                 WHERE i.id = ?1 AND i.closed_at IS NULL AND m.left_at IS NULL",
                [request.id.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((membership, mode, stored_hash, expiry, last_seen, created_at)) = stored else {
            return Err(RepositoryError::NotMember);
        };
        authenticate(&stored_hash, request.resume_token)?;
        let expiry = timestamp(expiry)?;
        let last_seen = timestamp(last_seen)?;
        let effective_now = request.now.max(last_seen);
        let state = InstanceState::at(None, expiry, effective_now);
        let renewed = renew_lease(
            state,
            expiry,
            effective_now,
            request.lease_policy.lease_duration(),
        )
        .map_err(|error| match error {
            psst_core::DomainError::LeaseExpired => RepositoryError::LeaseExpired,
            _ => RepositoryError::InvalidRequest,
        })?;
        transaction.execute(
            "UPDATE instances SET availability = ?2, availability_source = ?3,
                    availability_observed_at = ?4, lease_expires_at = ?5, last_seen_at = ?4
             WHERE id = ?1 AND closed_at IS NULL",
            params![
                request.id.as_str(),
                availability(request.availability),
                availability_source(request.availability_source),
                effective_now.as_i64(),
                renewed.as_i64()
            ],
        )?;
        transaction.commit()?;
        Ok(InstanceRecord {
            id: request.id.clone(),
            membership_id: parse_id(&membership)?,
            mode: parse_mode(&mode)?,
            availability: request.availability,
            availability_source: request.availability_source,
            availability_observed_at: effective_now,
            lease_expires_at: renewed,
            last_seen_at: effective_now,
            created_at: timestamp(created_at)?,
            closed_at: None,
            heartbeat_interval: request.lease_policy.heartbeat_interval(),
            lease_duration: request.lease_policy.lease_duration(),
        })
    }

    /// Authenticates continuity, closes an expired predecessor, and creates a new
    /// instance in one transaction. A live predecessor is never preempted.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError::NameInUse`] for a live predecessor, hides invalid
    /// token material as [`RepositoryError::NotMember`], or returns a stable storage error.
    pub fn resume_instance(
        &mut self,
        request: &ResumeInstance<'_>,
    ) -> Result<InstanceRecord, RepositoryError> {
        self.resume_instance_with_fault(request, false)
    }

    fn resume_instance_with_fault(
        &mut self,
        request: &ResumeInstance<'_>,
        fail_after_close: bool,
    ) -> Result<InstanceRecord, RepositoryError> {
        validate_observation(
            request.availability,
            request.availability_source,
            request.now,
        )?;
        validate_client(request.client_kind, request.hostname)?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_membership(&transaction, &request.membership_id)?;
        let predecessor: Option<(Vec<u8>, Option<i64>, i64)> = transaction
            .query_row(
                "SELECT resume_token_hash, closed_at, lease_expires_at FROM instances
                 WHERE membership_id = ?1 ORDER BY created_at DESC, id DESC LIMIT 1",
                [request.membership_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((stored_hash, closed_at, expiry)) = predecessor else {
            return Err(RepositoryError::NotMember);
        };
        authenticate(&stored_hash, request.resume_token)?;
        if closed_at.is_none() && request.now.as_i64() < expiry {
            return Err(RepositoryError::NameInUse);
        }
        transaction.execute(
            "UPDATE instances SET closed_at = COALESCE(closed_at, ?2)
             WHERE membership_id = ?1 AND closed_at IS NULL",
            params![request.membership_id.as_str(), request.now.as_i64()],
        )?;
        if fail_after_close {
            return Err(RepositoryError::InjectedFailure);
        }
        let record = insert_instance(
            &transaction,
            request.id.clone(),
            request.membership_id.clone(),
            request.mode,
            request.client_kind,
            request.hostname,
            request.resume_token,
            request.availability,
            request.availability_source,
            request.now,
            request.lease_policy,
        )?;
        transaction.commit()?;
        Ok(record)
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_instance(
    transaction: &Transaction<'_>,
    id: InstanceId,
    membership_id: MembershipId,
    mode: AgentMode,
    client_kind: &str,
    hostname: Option<&str>,
    resume_token: &ResumeToken,
    observed: Availability,
    source: AvailabilitySource,
    now: UnixMillis,
    policy: LeasePolicy,
) -> Result<InstanceRecord, RepositoryError> {
    let expiry = now
        .checked_add(policy.lease_duration())
        .ok_or(RepositoryError::InvalidRequest)?;
    let hash = token_hash(resume_token);
    transaction.execute(
        "INSERT INTO instances(
             id, membership_id, mode, client_kind, hostname, resume_token_hash,
             availability, availability_source, availability_observed_at,
             lease_expires_at, last_seen_at, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?9, ?9)",
        params![
            id.as_str(),
            membership_id.as_str(),
            mode_name(mode),
            client_kind,
            hostname,
            hash,
            availability(observed),
            availability_source(source),
            now.as_i64(),
            expiry.as_i64()
        ],
    )?;
    Ok(InstanceRecord {
        id,
        membership_id,
        mode,
        availability: observed,
        availability_source: source,
        availability_observed_at: now,
        lease_expires_at: expiry,
        last_seen_at: now,
        created_at: now,
        closed_at: None,
        heartbeat_interval: policy.heartbeat_interval(),
        lease_duration: policy.lease_duration(),
    })
}

fn require_active_membership(
    transaction: &Transaction<'_>,
    membership_id: &MembershipId,
) -> Result<(), RepositoryError> {
    let active: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM memberships m JOIN squads s ON s.id = m.squad_id
         WHERE m.id = ?1 AND m.left_at IS NULL AND s.state = 'active')",
        [membership_id.as_str()],
        |row| row.get(0),
    )?;
    if active {
        Ok(())
    } else {
        Err(RepositoryError::NotMember)
    }
}

fn authenticate(stored: &[u8], supplied: &ResumeToken) -> Result<(), RepositoryError> {
    let supplied = token_hash(supplied);
    if stored.len() == supplied.len() && bool::from(stored.ct_eq(supplied.as_slice())) {
        Ok(())
    } else {
        Err(RepositoryError::NotMember)
    }
}

fn token_hash(token: &ResumeToken) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(TOKEN_HASH_DOMAIN);
    digest.update(token.expose_encoded().as_bytes());
    digest.finalize().into()
}

fn validate_observation(
    availability: Availability,
    source: AvailabilitySource,
    observed_at: UnixMillis,
) -> Result<(), RepositoryError> {
    AvailabilityObservation::new(availability, source, observed_at)
        .map(|_| ())
        .map_err(|_| RepositoryError::InvalidRequest)
}

fn validate_client(client_kind: &str, hostname: Option<&str>) -> Result<(), RepositoryError> {
    if client_kind.trim().is_empty() || hostname.is_some_and(|value| value.trim().is_empty()) {
        Err(RepositoryError::InvalidRequest)
    } else {
        Ok(())
    }
}

const fn mode_name(value: AgentMode) -> &'static str {
    match value {
        AgentMode::Cooperative => "cooperative",
        AgentMode::Scheduled => "scheduled",
        AgentMode::Harnessed => "harnessed",
    }
}

fn parse_mode(value: &str) -> Result<AgentMode, RepositoryError> {
    match value {
        "cooperative" => Ok(AgentMode::Cooperative),
        "scheduled" => Ok(AgentMode::Scheduled),
        "harnessed" => Ok(AgentMode::Harnessed),
        _ => Err(RepositoryError::InvalidStoredData),
    }
}

const fn availability(value: Availability) -> &'static str {
    match value {
        Availability::Idle => "idle",
        Availability::Busy => "busy",
        Availability::Blocked => "blocked",
        Availability::Unknown => "unknown",
    }
}

const fn availability_source(value: AvailabilitySource) -> &'static str {
    match value {
        AvailabilitySource::SessionLifecycle => "session_lifecycle",
        AvailabilitySource::McpConnection => "mcp_connection",
        AvailabilitySource::ToolActivity => "tool_activity",
        AvailabilitySource::AgentReported => "agent_reported",
        AvailabilitySource::Unknown => "unknown",
    }
}

fn timestamp(value: i64) -> Result<UnixMillis, RepositoryError> {
    UnixMillis::new(value).map_err(|_| RepositoryError::InvalidStoredData)
}

fn parse_id(value: &str) -> Result<MembershipId, RepositoryError> {
    MembershipId::new(value).map_err(|_| RepositoryError::InvalidStoredData)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use psst_core::{AgentId, MemberName, Mission, Role, SquadId, SquadName};
    use tempfile::TempDir;

    use super::*;
    use crate::{CreateSquad, JoinMembership};

    fn token(alternate: bool) -> ResumeToken {
        let encoded = if alternate {
            "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE"
        } else {
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        };
        ResumeToken::from_encoded(encoded).unwrap()
    }

    fn setup(path: &std::path::Path) -> MembershipId {
        let mut store = Store::open(path).unwrap();
        store
            .create_squad(&CreateSquad {
                id: SquadId::new("sqd_alpha").unwrap(),
                name: SquadName::new("alpha").unwrap(),
                mission: Mission::new("Build reliable software").unwrap(),
                created_at: UnixMillis::new(1).unwrap(),
            })
            .unwrap();
        let membership = MembershipId::new("mem_alice").unwrap();
        store
            .join(&JoinMembership {
                squad_name: SquadName::new("alpha").unwrap(),
                mission_if_missing: None,
                squad_id_if_missing: SquadId::new("sqd_unused").unwrap(),
                agent_id: AgentId::new("agt_alice").unwrap(),
                membership_id: membership.clone(),
                member_name: MemberName::new("alice").unwrap(),
                role: Role::new("engineer").unwrap(),
                joined_at: UnixMillis::new(2).unwrap(),
            })
            .unwrap();
        membership
    }

    fn claim<'a>(id: &str, membership_id: &MembershipId, now: i64) -> ClaimInstance<'a> {
        ClaimInstance {
            id: InstanceId::new(id).unwrap(),
            membership_id: membership_id.clone(),
            mode: AgentMode::Cooperative,
            client_kind: "test-adapter",
            hostname: Some("test-host"),
            availability: Availability::Unknown,
            availability_source: AvailabilitySource::Unknown,
            now: UnixMillis::new(now).unwrap(),
            lease_policy: LeasePolicy::default(),
        }
    }

    fn resume<'a>(
        id: &str,
        membership_id: &MembershipId,
        resume_token: &'a ResumeToken,
        now: i64,
    ) -> ResumeInstance<'a> {
        ResumeInstance {
            id: InstanceId::new(id).unwrap(),
            membership_id: membership_id.clone(),
            mode: AgentMode::Harnessed,
            client_kind: "test-adapter",
            hostname: None,
            resume_token,
            availability: Availability::Idle,
            availability_source: AvailabilitySource::SessionLifecycle,
            now: UnixMillis::new(now).unwrap(),
            lease_policy: LeasePolicy::default(),
        }
    }

    #[test]
    fn default_claim_advertises_ten_and_thirty_seconds_and_stores_only_hash() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let membership = setup(&path);
        let mut store = Store::open(path).unwrap();
        let outcome = store
            .claim_instance(&claim("ins_first", &membership, 100))
            .unwrap();
        let record = outcome.instance();
        let secret = outcome.resume_token();
        assert_eq!(record.heartbeat_interval, Duration::from_secs(10));
        assert_eq!(record.lease_duration, Duration::from_secs(30));
        assert_eq!(record.lease_expires_at.as_i64(), 30_100);
        let stored: Vec<u8> = store
            .connection
            .query_row("SELECT resume_token_hash FROM instances", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(stored.len(), 32);
        assert_ne!(stored, secret.expose_encoded().as_bytes());
        let dump: String = store
            .connection
            .query_row("SELECT hex(resume_token_hash) FROM instances", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(!dump.contains(secret.expose_encoded()));
    }

    #[test]
    fn invalid_claim_observation_is_rejected_without_writing() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let membership = setup(&path);
        let mut store = Store::open(path).unwrap();
        let mut request = claim("ins_first", &membership, 100);
        request.availability = Availability::Idle;
        request.availability_source = AvailabilitySource::Unknown;
        assert!(matches!(
            store.claim_instance(&request),
            Err(RepositoryError::InvalidRequest)
        ));
        let count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM instances", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn independent_connections_allow_only_one_initial_owner() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let membership = setup(&path);
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = ["one", "two"]
            .into_iter()
            .map(|suffix| {
                let path = path.clone();
                let membership = membership.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut store = Store::open(path).unwrap();
                    barrier.wait();
                    store.claim_instance(&claim(&format!("ins_{suffix}"), &membership, 100))
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|item| item.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(RepositoryError::NameInUse)))
                .count(),
            1
        );
    }

    #[test]
    fn heartbeat_boundaries_and_clock_rollback_are_deterministic() {
        for (now, expected) in [(30_099, true), (30_100, false), (30_101, false)] {
            let directory = TempDir::new().unwrap();
            let path = directory.path().join("psst.db");
            let membership = setup(&path);
            let mut store = Store::open(path).unwrap();
            let outcome = store
                .claim_instance(&claim("ins_first", &membership, 100))
                .unwrap();
            let secret = outcome.resume_token();
            let request = HeartbeatInstance {
                id: &InstanceId::new("ins_first").unwrap(),
                resume_token: secret,
                availability: Availability::Busy,
                availability_source: AvailabilitySource::ToolActivity,
                now: UnixMillis::new(now).unwrap(),
                lease_policy: LeasePolicy::default(),
            };
            assert_eq!(store.heartbeat_instance(&request).is_ok(), expected);
        }

        let directory = TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let membership = setup(&path);
        let mut store = Store::open(path).unwrap();
        let outcome = store
            .claim_instance(&claim("ins_first", &membership, 1_000))
            .unwrap();
        let secret = outcome.resume_token();
        let id = InstanceId::new("ins_first").unwrap();
        let rolled_back = store
            .heartbeat_instance(&HeartbeatInstance {
                id: &id,
                resume_token: secret,
                availability: Availability::Idle,
                availability_source: AvailabilitySource::ToolActivity,
                now: UnixMillis::new(900).unwrap(),
                lease_policy: LeasePolicy::default(),
            })
            .unwrap();
        assert_eq!(rolled_back.last_seen_at.as_i64(), 1_000);
        assert_eq!(rolled_back.lease_expires_at.as_i64(), 31_000);
    }

    #[test]
    fn invalid_heartbeat_observation_is_rejected_without_mutating_lease() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let membership = setup(&path);
        let mut store = Store::open(path).unwrap();
        let outcome = store
            .claim_instance(&claim("ins_first", &membership, 100))
            .unwrap();
        let id = InstanceId::new("ins_first").unwrap();
        let request = HeartbeatInstance {
            id: &id,
            resume_token: outcome.resume_token(),
            availability: Availability::Unknown,
            availability_source: AvailabilitySource::ToolActivity,
            now: UnixMillis::new(1_000).unwrap(),
            lease_policy: LeasePolicy::default(),
        };
        assert!(matches!(
            store.heartbeat_instance(&request),
            Err(RepositoryError::InvalidRequest)
        ));
        let stored: (String, String, i64, i64, i64) = store
            .connection
            .query_row(
                "SELECT availability, availability_source, availability_observed_at,
                        lease_expires_at, last_seen_at FROM instances WHERE id = 'ins_first'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            stored,
            ("unknown".into(), "unknown".into(), 100, 30_100, 100)
        );
    }

    #[test]
    fn resume_requires_token_and_expiry_and_creates_new_instance() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let membership = setup(&path);
        let mut store = Store::open(path).unwrap();
        let outcome = store
            .claim_instance(&claim("ins_first", &membership, 100))
            .unwrap();
        let valid = outcome.resume_token();
        let invalid = token(true);
        assert!(matches!(
            store.resume_instance(&resume("ins_live", &membership, valid, 30_099)),
            Err(RepositoryError::NameInUse)
        ));
        assert!(matches!(
            store.resume_instance(&resume("ins_bad", &membership, &invalid, 30_100)),
            Err(RepositoryError::NotMember)
        ));
        let resumed = store
            .resume_instance(&resume("ins_second", &membership, valid, 30_100))
            .unwrap();
        assert_eq!(resumed.id.as_str(), "ins_second");
        let predecessor_closed: i64 = store
            .connection
            .query_row(
                "SELECT closed_at FROM instances WHERE id = 'ins_first'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(predecessor_closed, 30_100);
    }

    #[test]
    fn failed_resume_rolls_back_predecessor_close_and_new_instance() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let membership = setup(&path);
        let mut store = Store::open(path).unwrap();
        let outcome = store
            .claim_instance(&claim("ins_first", &membership, 100))
            .unwrap();
        let valid = outcome.resume_token();
        assert!(matches!(
            store.resume_instance_with_fault(
                &resume("ins_second", &membership, valid, 30_100),
                true
            ),
            Err(RepositoryError::InjectedFailure)
        ));
        let (closed, count): (Option<i64>, i64) = store
            .connection
            .query_row(
                "SELECT closed_at, (SELECT COUNT(*) FROM instances) FROM instances WHERE id = 'ins_first'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((closed, count), (None, 1));
    }

    #[test]
    fn invalid_resume_observation_is_rejected_without_closing_predecessor() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let membership = setup(&path);
        let mut store = Store::open(path).unwrap();
        let outcome = store
            .claim_instance(&claim("ins_first", &membership, 100))
            .unwrap();
        let mut request = resume("ins_second", &membership, outcome.resume_token(), 30_100);
        request.availability_source = AvailabilitySource::Unknown;
        assert!(matches!(
            store.resume_instance(&request),
            Err(RepositoryError::InvalidRequest)
        ));
        let (closed, count): (Option<i64>, i64) = store
            .connection
            .query_row(
                "SELECT closed_at, (SELECT COUNT(*) FROM instances)
                 FROM instances WHERE id = 'ins_first'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((closed, count), (None, 1));
    }

    #[test]
    fn valid_resume_survives_store_restart() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("psst.db");
        let membership = setup(&path);
        let mut store = Store::open(&path).unwrap();
        let outcome = store
            .claim_instance(&claim("ins_first", &membership, 100))
            .unwrap();
        let valid = outcome.resume_token().clone();
        drop(store);
        let mut reopened = Store::open(path).unwrap();
        let resumed = reopened
            .resume_instance(&resume("ins_second", &membership, &valid, 30_100))
            .unwrap();
        assert_eq!(resumed.id.as_str(), "ins_second");
    }
}
