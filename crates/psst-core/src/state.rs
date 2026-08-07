use crate::{DomainError, UnixMillis};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentMode {
    Cooperative,
    Scheduled,
    Harnessed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SquadState {
    Active,
    Archived,
}

impl SquadState {
    /// Archives an active squad.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidStateTransition`] if already archived.
    pub fn archive(&mut self) -> Result<(), DomainError> {
        if *self == Self::Archived {
            return Err(DomainError::InvalidStateTransition {
                entity: "squad",
                action: "archive",
            });
        }
        *self = Self::Archived;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MembershipState {
    Joined,
    Left,
}

impl MembershipState {
    /// Leaves a joined membership.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidStateTransition`] if already left.
    pub fn leave(&mut self) -> Result<(), DomainError> {
        if *self == Self::Left {
            return Err(DomainError::InvalidStateTransition {
                entity: "membership",
                action: "leave",
            });
        }
        *self = Self::Left;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstanceState {
    Online,
    Expired,
    Closed,
}

impl InstanceState {
    #[must_use]
    pub const fn at(
        closed_at: Option<UnixMillis>,
        lease_expires_at: UnixMillis,
        now: UnixMillis,
    ) -> Self {
        if closed_at.is_some() {
            Self::Closed
        } else if now.as_i64() >= lease_expires_at.as_i64() {
            Self::Expired
        } else {
            Self::Online
        }
    }

    /// Closes an online instance.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidStateTransition`] unless the instance is online.
    pub fn close(&mut self) -> Result<(), DomainError> {
        if *self != Self::Online {
            return Err(DomainError::InvalidStateTransition {
                entity: "instance",
                action: "close",
            });
        }
        *self = Self::Closed;
        Ok(())
    }
}

/// Renews an online lease without allowing wall-clock rollback to shorten it.
///
/// # Errors
///
/// Returns [`DomainError::LeaseExpired`] at or after expiry, and an invalid-transition error
/// when called for a closed instance.
pub fn renew_lease(
    state: InstanceState,
    current_expiry: UnixMillis,
    now: UnixMillis,
    duration: Duration,
) -> Result<UnixMillis, DomainError> {
    if state == InstanceState::Closed {
        return Err(DomainError::InvalidStateTransition {
            entity: "instance",
            action: "heartbeat",
        });
    }
    if state == InstanceState::Expired || now >= current_expiry {
        return Err(DomainError::LeaseExpired);
    }
    let candidate = now.checked_add(duration).ok_or_else(|| {
        DomainError::InvalidValue(crate::InvalidValue::new("lease", "overflows timestamp"))
    })?;
    Ok(candidate.max(current_expiry))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageState {
    Pending,
    Acknowledged,
}

impl MessageState {
    /// Acknowledges a pending message.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidStateTransition`] if already acknowledged.
    pub fn acknowledge(&mut self) -> Result<(), DomainError> {
        if *self == Self::Acknowledged {
            return Err(DomainError::InvalidStateTransition {
                entity: "message",
                action: "acknowledge",
            });
        }
        *self = Self::Acknowledged;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Availability {
    Idle,
    Busy,
    Blocked,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvailabilitySource {
    SessionLifecycle,
    McpConnection,
    ToolActivity,
    AgentReported,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AvailabilityObservation {
    availability: Availability,
    source: AvailabilitySource,
    observed_at: UnixMillis,
}

impl AvailabilityObservation {
    /// Creates a consistent advisory availability observation.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidValue`] if unknown availability is paired with a known
    /// source, or known availability is paired with an unknown source.
    pub fn new(
        availability: Availability,
        source: AvailabilitySource,
        observed_at: UnixMillis,
    ) -> Result<Self, DomainError> {
        if (availability == Availability::Unknown) != (source == AvailabilitySource::Unknown) {
            return Err(DomainError::InvalidValue(crate::InvalidValue::new(
                "availability",
                "availability and observation source must both be known or both be unknown",
            )));
        }
        Ok(Self {
            availability,
            source,
            observed_at,
        })
    }

    #[must_use]
    pub const fn availability(self) -> Availability {
        self.availability
    }

    #[must_use]
    pub const fn source(self) -> AvailabilitySource {
        self.source
    }

    #[must_use]
    pub const fn observed_at(self) -> UnixMillis {
        self.observed_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_transitions_are_rejected() {
        let mut squad = SquadState::Active;
        squad.archive().unwrap();
        assert!(squad.archive().is_err());
        let mut membership = MembershipState::Joined;
        membership.leave().unwrap();
        assert!(membership.leave().is_err());
        let mut instance = InstanceState::Expired;
        assert!(instance.close().is_err());
    }

    #[test]
    fn lease_expires_at_exact_boundary() {
        let expiry = UnixMillis::new(30).unwrap();
        assert_eq!(
            InstanceState::at(None, expiry, UnixMillis::new(29).unwrap()),
            InstanceState::Online
        );
        assert_eq!(
            InstanceState::at(None, expiry, expiry),
            InstanceState::Expired
        );
        assert_eq!(
            InstanceState::at(
                Some(UnixMillis::new(10).unwrap()),
                expiry,
                UnixMillis::new(1).unwrap()
            ),
            InstanceState::Closed
        );
    }

    #[test]
    fn lease_renewal_never_shortens_and_cannot_revive() {
        let expiry = UnixMillis::new(30_000).unwrap();
        assert_eq!(
            renew_lease(
                InstanceState::Online,
                expiry,
                UnixMillis::new(1_000).unwrap(),
                Duration::from_secs(10)
            )
            .unwrap(),
            expiry
        );
        assert_eq!(
            renew_lease(
                InstanceState::Online,
                expiry,
                UnixMillis::new(25_000).unwrap(),
                Duration::from_secs(30)
            )
            .unwrap(),
            UnixMillis::new(55_000).unwrap()
        );
        assert_eq!(
            renew_lease(
                InstanceState::Expired,
                expiry,
                expiry,
                Duration::from_secs(30)
            ),
            Err(DomainError::LeaseExpired)
        );
    }

    #[test]
    fn acknowledgement_is_terminal() {
        let mut message = MessageState::Pending;
        message.acknowledge().unwrap();
        assert_eq!(message, MessageState::Acknowledged);
        assert!(message.acknowledge().is_err());
    }

    #[test]
    fn availability_never_disguises_unknown_as_known() {
        let now = UnixMillis::new(1).unwrap();
        assert!(
            AvailabilityObservation::new(Availability::Unknown, AvailabilitySource::Unknown, now)
                .is_ok()
        );
        assert!(
            AvailabilityObservation::new(
                Availability::Idle,
                AvailabilitySource::SessionLifecycle,
                now
            )
            .is_ok()
        );
        assert!(
            AvailabilityObservation::new(Availability::Idle, AvailabilitySource::Unknown, now)
                .is_err()
        );
        assert!(
            AvailabilityObservation::new(
                Availability::Unknown,
                AvailabilitySource::AgentReported,
                now
            )
            .is_err()
        );
    }
}
