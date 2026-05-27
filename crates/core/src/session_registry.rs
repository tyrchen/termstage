//! Session registry for backend-owned terminal sessions.
//!
//! The registry is intended to be owned by the future gateway actor. It is a
//! plain actor-local map, not a concurrent global store.

use std::collections::HashMap;

use thiserror::Error;

use crate::{backend::BackendSessionRef, protocol::SessionName};

/// Registered `termstage` session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    termstage_session: SessionName,
    backend: BackendSessionRef,
}

impl SessionRecord {
    /// Creates a session registry record.
    #[must_use]
    pub const fn new(termstage_session: SessionName, backend: BackendSessionRef) -> Self {
        Self {
            termstage_session,
            backend,
        }
    }

    /// Returns the `termstage` session id.
    #[must_use]
    pub const fn termstage_session(&self) -> &SessionName {
        &self.termstage_session
    }

    /// Returns the backend session reference.
    #[must_use]
    pub const fn backend(&self) -> &BackendSessionRef {
        &self.backend
    }
}

/// Session registry failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SessionRegistryError {
    /// The session is already registered.
    #[error("termstage session is already registered")]
    AlreadyRegistered {
        /// Session id.
        session: SessionName,
    },
    /// The session is not registered.
    #[error("termstage session is not registered")]
    NotRegistered {
        /// Session id.
        session: SessionName,
    },
}

/// Actor-owned registry from `termstage` session ids to backend references.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    records: HashMap<SessionName, SessionRecord>,
}

impl SessionRegistry {
    /// Creates an empty session registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionRegistryError::AlreadyRegistered`] when the session id
    /// already exists.
    pub fn register(&mut self, record: SessionRecord) -> Result<(), SessionRegistryError> {
        let key = record.termstage_session.clone();
        if self.records.contains_key(&key) {
            return Err(SessionRegistryError::AlreadyRegistered { session: key });
        }
        self.records.insert(key, record);
        Ok(())
    }

    /// Looks up a registered session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionRegistryError::NotRegistered`] when the session id does
    /// not exist.
    pub fn get(&self, session: &SessionName) -> Result<&SessionRecord, SessionRegistryError> {
        self.records
            .get(session)
            .ok_or_else(|| SessionRegistryError::NotRegistered {
                session: session.clone(),
            })
    }

    /// Removes a registered session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionRegistryError::NotRegistered`] when the session id does
    /// not exist.
    pub fn remove(&mut self, session: &SessionName) -> Result<SessionRecord, SessionRegistryError> {
        self.records
            .remove(session)
            .ok_or_else(|| SessionRegistryError::NotRegistered {
                session: session.clone(),
            })
    }

    /// Returns the number of registered sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendKind, BackendPaneId, BackendSessionRef, BackendWindowId};

    fn record(name: &str) -> anyhow::Result<SessionRecord> {
        let session = SessionName::new(name)?;
        let backend = BackendSessionRef::new(
            BackendKind::Tmux,
            session.clone(),
            BackendWindowId::new("0")?,
            BackendPaneId::new("%1")?,
        );
        Ok(SessionRecord::new(session, backend))
    }

    #[test]
    fn test_should_register_and_remove_session() -> anyhow::Result<()> {
        let mut registry = SessionRegistry::new();
        let session = SessionName::new("demo")?;
        registry.register(record("demo")?)?;

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get(&session)?.backend().session().as_str(), "demo");
        assert_eq!(
            registry.remove(&session)?.termstage_session().as_str(),
            "demo"
        );
        assert!(registry.is_empty());
        Ok(())
    }

    #[test]
    fn test_should_reject_duplicate_session() -> anyhow::Result<()> {
        let mut registry = SessionRegistry::new();
        registry.register(record("demo")?)?;
        let error = match registry.register(record("demo")?) {
            Ok(()) => anyhow::bail!("duplicate session should fail"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            SessionRegistryError::AlreadyRegistered { session } if session.as_str() == "demo"
        ));
        Ok(())
    }

    #[test]
    fn test_should_reject_missing_session() -> anyhow::Result<()> {
        let registry = SessionRegistry::new();
        let error = match registry.get(&SessionName::new("missing")?) {
            Ok(_record) => anyhow::bail!("missing session should fail"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            SessionRegistryError::NotRegistered { session } if session.as_str() == "missing"
        ));
        Ok(())
    }
}
