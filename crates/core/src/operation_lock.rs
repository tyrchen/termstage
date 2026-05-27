//! Level 1 operation lock for terminal controllers.
//!
//! This lock is enforced by `termstage` before input bytes or semantic
//! operations are forwarded to a backend pane. Backends may add their own
//! authoritative locks in a later level.

use std::{
    collections::HashMap,
    num::NonZeroU64,
    time::{Duration, Instant},
};

use thiserror::Error;

use crate::protocol::SessionName;

/// Identifier for a controller connected to `termstage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ControllerId(NonZeroU64);

impl ControllerId {
    /// Creates a non-zero controller id.
    ///
    /// # Errors
    ///
    /// Returns [`OperationLockError::InvalidControllerId`] when `value` is zero.
    pub fn new(value: u64) -> Result<Self, OperationLockError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(OperationLockError::InvalidControllerId)
    }

    /// Returns the controller id.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Controller surface kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControllerKind {
    /// Browser xterm.js controller.
    Browser,
    /// Agent Semantic Operations API controller.
    Agent,
    /// Future transport controller.
    Transport,
}

/// Unique controller reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ControllerRef {
    kind: ControllerKind,
    id: ControllerId,
}

impl ControllerRef {
    /// Creates a controller reference.
    #[must_use]
    pub const fn new(kind: ControllerKind, id: ControllerId) -> Self {
        Self { kind, id }
    }

    /// Returns the controller kind.
    #[must_use]
    pub const fn kind(self) -> ControllerKind {
        self.kind
    }

    /// Returns the controller id.
    #[must_use]
    pub const fn id(self) -> ControllerId {
        self.id
    }
}

/// Granted operation lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationLease {
    owner: ControllerRef,
    epoch: u64,
    expires_at: Instant,
}

impl OperationLease {
    /// Creates a granted lease.
    #[must_use]
    pub const fn new(owner: ControllerRef, epoch: u64, expires_at: Instant) -> Self {
        Self {
            owner,
            epoch,
            expires_at,
        }
    }

    /// Returns the lease owner.
    #[must_use]
    pub const fn owner(self) -> ControllerRef {
        self.owner
    }

    /// Returns the monotonically increasing session-local lease epoch.
    #[must_use]
    pub const fn epoch(self) -> u64 {
        self.epoch
    }

    /// Returns the instant when the lease expires.
    #[must_use]
    pub const fn expires_at(self) -> Instant {
        self.expires_at
    }

    /// Returns whether the lease is expired at `now`.
    #[must_use]
    pub fn is_expired_at(self, now: Instant) -> bool {
        self.expires_at <= now
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LockEntry {
    lease: OperationLease,
    next_epoch: u64,
}

/// Level 1 operation lock failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OperationLockError {
    /// Controller ids must be non-zero.
    #[error("controller id must be non-zero")]
    InvalidControllerId,
    /// Lease TTL must be greater than zero.
    #[error("operation lease ttl must be greater than zero")]
    InvalidLeaseTtl,
    /// Lease expiration overflowed.
    #[error("operation lease expiration overflowed")]
    LeaseExpirationOverflow,
    /// Another controller owns the session.
    #[error("operation lease is owned by another controller")]
    LeaseConflict {
        /// Session id.
        session: SessionName,
        /// Current owner.
        owner: ControllerRef,
    },
    /// The requested controller does not own the session.
    #[error("controller does not own the operation lease")]
    NotLeaseOwner {
        /// Session id.
        session: SessionName,
        /// Current owner, if the session has one.
        owner: Option<ControllerRef>,
    },
}

/// Actor-owned table of session operation locks.
#[derive(Debug, Default)]
pub struct OperationLockTable {
    entries: HashMap<SessionName, LockEntry>,
}

impl OperationLockTable {
    /// Creates an empty operation lock table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquires or renews a session operation lease.
    ///
    /// # Errors
    ///
    /// Returns [`OperationLockError::LeaseConflict`] when another non-expired
    /// controller owns the lease.
    pub fn acquire(
        &mut self,
        session: &SessionName,
        controller: ControllerRef,
        now: Instant,
        ttl: Duration,
    ) -> Result<OperationLease, OperationLockError> {
        if ttl.is_zero() {
            return Err(OperationLockError::InvalidLeaseTtl);
        }
        let expires_at = now
            .checked_add(ttl)
            .ok_or(OperationLockError::LeaseExpirationOverflow)?;

        let next_epoch = match self.entries.get(session) {
            Some(entry) if !entry.lease.is_expired_at(now) && entry.lease.owner() != controller => {
                return Err(OperationLockError::LeaseConflict {
                    session: session.clone(),
                    owner: entry.lease.owner(),
                });
            }
            Some(entry) => entry.next_epoch,
            None => 0,
        };
        let lease = OperationLease::new(controller, next_epoch, expires_at);
        let entry = LockEntry {
            lease,
            next_epoch: next_epoch.saturating_add(1),
        };
        self.entries.insert(session.clone(), entry);
        Ok(lease)
    }

    /// Validates that `controller` currently owns the session lease.
    ///
    /// # Errors
    ///
    /// Returns [`OperationLockError::NotLeaseOwner`] when the session is
    /// unlocked, expired, or owned by another controller.
    pub fn validate_owner(
        &mut self,
        session: &SessionName,
        controller: ControllerRef,
        now: Instant,
    ) -> Result<OperationLease, OperationLockError> {
        let Some(entry) = self.entries.get(session).copied() else {
            return Err(OperationLockError::NotLeaseOwner {
                session: session.clone(),
                owner: None,
            });
        };
        if entry.lease.is_expired_at(now) {
            self.entries.remove(session);
            return Err(OperationLockError::NotLeaseOwner {
                session: session.clone(),
                owner: None,
            });
        }
        if entry.lease.owner() != controller {
            return Err(OperationLockError::NotLeaseOwner {
                session: session.clone(),
                owner: Some(entry.lease.owner()),
            });
        }
        Ok(entry.lease)
    }

    /// Releases a lease owned by `controller`.
    ///
    /// # Errors
    ///
    /// Returns [`OperationLockError::NotLeaseOwner`] when the controller does
    /// not own the session lease.
    pub fn release(
        &mut self,
        session: &SessionName,
        controller: ControllerRef,
        now: Instant,
    ) -> Result<OperationLease, OperationLockError> {
        let lease = self.validate_owner(session, controller, now)?;
        self.entries.remove(session);
        Ok(lease)
    }

    /// Removes any tracked lease state for a session.
    pub fn remove_session(&mut self, session: &SessionName) {
        self.entries.remove(session);
    }

    /// Returns the current non-expired lease for a session.
    #[must_use]
    pub fn current(&mut self, session: &SessionName, now: Instant) -> Option<OperationLease> {
        let entry = self.entries.get(session).copied()?;
        if entry.lease.is_expired_at(now) {
            self.entries.remove(session);
            None
        } else {
            Some(entry.lease)
        }
    }

    /// Returns the number of tracked session locks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the lock table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser(id: u64) -> anyhow::Result<ControllerRef> {
        Ok(ControllerRef::new(
            ControllerKind::Browser,
            ControllerId::new(id)?,
        ))
    }

    fn agent(id: u64) -> anyhow::Result<ControllerRef> {
        Ok(ControllerRef::new(
            ControllerKind::Agent,
            ControllerId::new(id)?,
        ))
    }

    #[test]
    fn test_should_reject_zero_controller_id() {
        assert!(matches!(
            ControllerId::new(0),
            Err(OperationLockError::InvalidControllerId)
        ));
    }

    #[test]
    fn test_should_acquire_and_validate_owner() -> anyhow::Result<()> {
        let mut table = OperationLockTable::new();
        let session = SessionName::new("demo")?;
        let owner = browser(1)?;
        let now = Instant::now();

        let lease = table.acquire(&session, owner, now, Duration::from_secs(30))?;

        assert_eq!(lease.owner(), owner);
        assert_eq!(lease.epoch(), 0);
        assert_eq!(table.validate_owner(&session, owner, now)?, lease);
        Ok(())
    }

    #[test]
    fn test_should_reject_conflicting_owner_until_expired() -> anyhow::Result<()> {
        let mut table = OperationLockTable::new();
        let session = SessionName::new("demo")?;
        let first = browser(1)?;
        let second = agent(2)?;
        let now = Instant::now();
        table.acquire(&session, first, now, Duration::from_secs(5))?;

        let conflict = table.acquire(&session, second, now, Duration::from_secs(5));
        assert!(matches!(
            conflict,
            Err(OperationLockError::LeaseConflict { owner, .. }) if owner == first
        ));

        let replacement = table.acquire(
            &session,
            second,
            now + Duration::from_secs(6),
            Duration::from_secs(5),
        )?;
        assert_eq!(replacement.owner(), second);
        assert_eq!(replacement.epoch(), 1);
        Ok(())
    }

    #[test]
    fn test_should_renew_owner_and_increment_epoch() -> anyhow::Result<()> {
        let mut table = OperationLockTable::new();
        let session = SessionName::new("demo")?;
        let owner = browser(1)?;
        let now = Instant::now();
        table.acquire(&session, owner, now, Duration::from_secs(5))?;

        let renewed = table.acquire(
            &session,
            owner,
            now + Duration::from_secs(1),
            Duration::from_secs(5),
        )?;

        assert_eq!(renewed.owner(), owner);
        assert_eq!(renewed.epoch(), 1);
        Ok(())
    }

    #[test]
    fn test_should_release_only_owner() -> anyhow::Result<()> {
        let mut table = OperationLockTable::new();
        let session = SessionName::new("demo")?;
        let owner = browser(1)?;
        let other = agent(2)?;
        let now = Instant::now();
        table.acquire(&session, owner, now, Duration::from_secs(5))?;

        let error = table.release(&session, other, now);
        assert!(matches!(
            error,
            Err(OperationLockError::NotLeaseOwner { owner: Some(current), .. }) if current == owner
        ));

        let released = table.release(&session, owner, now)?;
        assert_eq!(released.owner(), owner);
        assert!(table.is_empty());
        Ok(())
    }

    #[test]
    fn test_should_drop_expired_current_lease() -> anyhow::Result<()> {
        let mut table = OperationLockTable::new();
        let session = SessionName::new("demo")?;
        let owner = browser(1)?;
        let now = Instant::now();
        table.acquire(&session, owner, now, Duration::from_secs(1))?;

        assert_eq!(table.current(&session, now + Duration::from_secs(2)), None);
        assert!(table.is_empty());
        Ok(())
    }

    #[test]
    fn test_should_remove_session_lease_state() -> anyhow::Result<()> {
        let mut table = OperationLockTable::new();
        let session = SessionName::new("demo")?;
        table.acquire(
            &session,
            browser(1)?,
            Instant::now(),
            Duration::from_secs(30),
        )?;

        table.remove_session(&session);

        assert!(table.is_empty());
        Ok(())
    }
}
