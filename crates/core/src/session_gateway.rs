//! Backend-session gateway.
//!
//! The gateway owns the session registry and Level 1 operation lock table. It is
//! the core boundary that browser and future Agent API surfaces will call before
//! any terminal write reaches a backend adapter.

use std::time::{Duration, Instant};

use bytes::Bytes;
use thiserror::Error;

use crate::{
    backend::{BackendAdapter, BackendError, BackendScreenSnapshot},
    operation_lock::{ControllerRef, OperationLease, OperationLockError, OperationLockTable},
    protocol::{SessionName, TerminalSize},
    session_registry::{SessionRecord, SessionRegistry, SessionRegistryError},
};

/// Backend-session gateway failure.
#[derive(Debug, Error)]
pub enum SessionGatewayError {
    /// Backend operation failed.
    #[error("backend operation failed")]
    Backend(#[from] BackendError),
    /// Session registry operation failed.
    #[error("session registry operation failed")]
    Registry(#[from] SessionRegistryError),
    /// Operation lock failed.
    #[error("operation lock failed")]
    Lock(#[from] OperationLockError),
}

/// Gateway that binds termstage sessions, operation locks, and a backend adapter.
#[derive(Debug)]
pub struct SessionGateway<B> {
    backend: B,
    registry: SessionRegistry,
    locks: OperationLockTable,
    lease_ttl: Duration,
}

impl<B> SessionGateway<B>
where
    B: BackendAdapter,
{
    /// Creates a backend-session gateway.
    #[must_use]
    pub fn new(backend: B, lease_ttl: Duration) -> Self {
        Self {
            backend,
            registry: SessionRegistry::new(),
            locks: OperationLockTable::new(),
            lease_ttl,
        }
    }

    /// Creates or finds a backend session and registers it under a `termstage`
    /// session id.
    ///
    /// # Errors
    ///
    /// Returns [`SessionGatewayError`] when the backend cannot resolve the
    /// session or the registry already contains the termstage id.
    pub async fn create_or_find_session(
        &mut self,
        termstage_session: SessionName,
        backend_session: SessionName,
        size: TerminalSize,
    ) -> Result<(), SessionGatewayError> {
        if self.registry.get(&termstage_session).is_ok() {
            return Err(SessionRegistryError::AlreadyRegistered {
                session: termstage_session,
            }
            .into());
        }
        let backend_ref = self
            .backend
            .create_or_find_session(&backend_session, size)
            .await?;
        self.registry
            .register(SessionRecord::new(termstage_session, backend_ref))?;
        Ok(())
    }

    /// Acquires or renews the Level 1 write lease for a session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionGatewayError`] when the session is unknown, the lease
    /// TTL is invalid, or another controller owns the lease.
    pub fn acquire_controller(
        &mut self,
        session: &SessionName,
        controller: ControllerRef,
        now: Instant,
    ) -> Result<OperationLease, SessionGatewayError> {
        self.registry.get(session)?;
        self.locks
            .acquire(session, controller, now, self.lease_ttl)
            .map_err(Into::into)
    }

    /// Releases the Level 1 write lease for a session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionGatewayError`] when the controller does not own the
    /// session lease.
    pub fn release_controller(
        &mut self,
        session: &SessionName,
        controller: ControllerRef,
        now: Instant,
    ) -> Result<OperationLease, SessionGatewayError> {
        self.registry.get(session)?;
        self.locks
            .release(session, controller, now)
            .map_err(Into::into)
    }

    /// Writes terminal bytes to the backend when `controller` owns the lease.
    ///
    /// # Errors
    ///
    /// Returns [`SessionGatewayError`] when the session is missing, the
    /// controller is not the current owner, or the backend write fails.
    pub async fn write_input(
        &mut self,
        session: &SessionName,
        controller: ControllerRef,
        bytes: Bytes,
        now: Instant,
    ) -> Result<OperationLease, SessionGatewayError> {
        let record = self.registry.get(session)?.clone();
        let lease = self.locks.validate_owner(session, controller, now)?;
        self.backend.write_input(record.backend(), bytes).await?;
        Ok(lease)
    }

    /// Resizes the backend pane when `controller` owns the lease.
    ///
    /// # Errors
    ///
    /// Returns [`SessionGatewayError`] when the session is missing, the
    /// controller is not the current owner, or the backend resize fails.
    pub async fn resize(
        &mut self,
        session: &SessionName,
        controller: ControllerRef,
        size: TerminalSize,
        now: Instant,
    ) -> Result<OperationLease, SessionGatewayError> {
        let record = self.registry.get(session)?.clone();
        let lease = self.locks.validate_owner(session, controller, now)?;
        self.backend.resize(record.backend(), size).await?;
        Ok(lease)
    }

    /// Reads the backend pane screen. Read operations do not require ownership.
    ///
    /// # Errors
    ///
    /// Returns [`SessionGatewayError`] when the session is missing or the
    /// backend cannot provide screen state.
    pub async fn read_screen(
        &mut self,
        session: &SessionName,
    ) -> Result<BackendScreenSnapshot, SessionGatewayError> {
        let record = self.registry.get(session)?.clone();
        self.backend
            .read_screen(record.backend())
            .await
            .map_err(Into::into)
    }

    /// Closes a registered backend session and removes it from the registry.
    ///
    /// # Errors
    ///
    /// Returns [`SessionGatewayError`] when the session is missing or the
    /// backend close fails.
    pub async fn close_session(
        &mut self,
        session: &SessionName,
    ) -> Result<SessionRecord, SessionGatewayError> {
        let record = self.registry.get(session)?.clone();
        self.backend.close_session(record.backend()).await?;
        self.registry.remove(session)?;
        self.locks.remove_session(session);
        Ok(record)
    }

    /// Returns a shared reference to the gateway registry.
    #[must_use]
    pub const fn registry(&self) -> &SessionRegistry {
        &self.registry
    }

    /// Returns a shared reference to the gateway lock table.
    #[must_use]
    pub const fn locks(&self) -> &OperationLockTable {
        &self.locks
    }

    /// Consumes the gateway and returns its backend adapter.
    #[must_use]
    pub fn into_backend(self) -> B {
        self.backend
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backend::{BackendKind, BackendPaneId, BackendSessionRef, BackendWindowId},
        operation_lock::{ControllerId, ControllerKind},
        protocol::SafeMessage,
    };

    #[derive(Debug, Default)]
    struct FakeBackend {
        created: Vec<SessionName>,
        writes: Vec<Bytes>,
        resizes: Vec<TerminalSize>,
        closed: Vec<SessionName>,
        fail_close: bool,
    }

    impl FakeBackend {
        fn reference(session: &SessionName) -> Result<BackendSessionRef, BackendError> {
            Ok(BackendSessionRef::new(
                BackendKind::Tmux,
                session.clone(),
                BackendWindowId::new("0")?,
                BackendPaneId::new("%1")?,
            ))
        }
    }

    impl BackendAdapter for FakeBackend {
        async fn create_or_find_session(
            &mut self,
            session: &SessionName,
            _size: TerminalSize,
        ) -> Result<BackendSessionRef, BackendError> {
            self.created.push(session.clone());
            Self::reference(session)
        }

        async fn write_input(
            &mut self,
            _target: &BackendSessionRef,
            bytes: Bytes,
        ) -> Result<(), BackendError> {
            self.writes.push(bytes);
            Ok(())
        }

        async fn resize(
            &mut self,
            _target: &BackendSessionRef,
            size: TerminalSize,
        ) -> Result<(), BackendError> {
            self.resizes.push(size);
            Ok(())
        }

        async fn read_screen(
            &mut self,
            target: &BackendSessionRef,
        ) -> Result<BackendScreenSnapshot, BackendError> {
            Ok(BackendScreenSnapshot::new(
                TerminalSize::new(80, 24)?,
                0,
                0,
                vec![format!("session={}", target.session().as_str())],
            ))
        }

        async fn close_session(&mut self, target: &BackendSessionRef) -> Result<(), BackendError> {
            if self.fail_close {
                return Err(BackendError::Operation(SafeMessage::from_static(
                    "close failed",
                )));
            }
            self.closed.push(target.session().clone());
            Ok(())
        }
    }

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

    async fn gateway() -> anyhow::Result<SessionGateway<FakeBackend>> {
        let mut gateway = SessionGateway::new(FakeBackend::default(), Duration::from_secs(30));
        gateway
            .create_or_find_session(
                SessionName::new("demo")?,
                SessionName::new("backend-demo")?,
                TerminalSize::new(80, 24)?,
            )
            .await?;
        Ok(gateway)
    }

    #[tokio::test]
    async fn test_should_write_and_resize_when_controller_owns_lease() -> anyhow::Result<()> {
        let mut gateway = gateway().await?;
        let session = SessionName::new("demo")?;
        let owner = browser(1)?;
        let now = Instant::now();
        gateway.acquire_controller(&session, owner, now)?;

        gateway
            .write_input(&session, owner, Bytes::from_static(b"echo ok\n"), now)
            .await?;
        gateway
            .resize(&session, owner, TerminalSize::new(100, 30)?, now)
            .await?;
        let backend = gateway.into_backend();

        assert_eq!(backend.writes, [Bytes::from_static(b"echo ok\n")]);
        assert_eq!(backend.resizes, [TerminalSize::new(100, 30)?]);
        Ok(())
    }

    #[tokio::test]
    async fn test_should_reject_write_from_non_owner() -> anyhow::Result<()> {
        let mut gateway = gateway().await?;
        let session = SessionName::new("demo")?;
        let owner = browser(1)?;
        let other = agent(2)?;
        let now = Instant::now();
        gateway.acquire_controller(&session, owner, now)?;

        let error = gateway
            .write_input(&session, other, Bytes::from_static(b"blocked"), now)
            .await;

        assert!(matches!(
            error,
            Err(SessionGatewayError::Lock(OperationLockError::NotLeaseOwner { owner: Some(current), .. }))
                if current == owner
        ));
        let backend = gateway.into_backend();
        assert!(backend.writes.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_should_allow_read_without_lease() -> anyhow::Result<()> {
        let mut gateway = gateway().await?;
        let snapshot = gateway.read_screen(&SessionName::new("demo")?).await?;

        assert_eq!(snapshot.lines(), ["session=backend-demo"]);
        Ok(())
    }

    #[tokio::test]
    async fn test_should_close_registered_session() -> anyhow::Result<()> {
        let mut gateway = gateway().await?;
        let session = SessionName::new("demo")?;
        let owner = browser(1)?;
        let now = Instant::now();
        gateway.acquire_controller(&session, owner, now)?;

        let record = gateway.close_session(&session).await?;

        assert_eq!(record.termstage_session().as_str(), "demo");
        assert!(gateway.registry().is_empty());
        assert!(gateway.locks().is_empty());
        let backend = gateway.into_backend();
        assert_eq!(backend.closed, [SessionName::new("backend-demo")?]);
        Ok(())
    }

    #[tokio::test]
    async fn test_should_reject_duplicate_session_before_backend_call() -> anyhow::Result<()> {
        let mut gateway = gateway().await?;
        let error = gateway
            .create_or_find_session(
                SessionName::new("demo")?,
                SessionName::new("other-backend")?,
                TerminalSize::new(80, 24)?,
            )
            .await;

        assert!(matches!(
            error,
            Err(SessionGatewayError::Registry(
                SessionRegistryError::AlreadyRegistered { .. }
            ))
        ));
        let backend = gateway.into_backend();
        assert_eq!(backend.created, [SessionName::new("backend-demo")?]);
        Ok(())
    }

    #[tokio::test]
    async fn test_should_keep_session_registered_when_backend_close_fails() -> anyhow::Result<()> {
        let backend = FakeBackend {
            fail_close: true,
            ..FakeBackend::default()
        };
        let mut gateway = SessionGateway::new(backend, Duration::from_secs(30));
        let session = SessionName::new("demo")?;
        gateway
            .create_or_find_session(
                session.clone(),
                SessionName::new("backend-demo")?,
                TerminalSize::new(80, 24)?,
            )
            .await?;

        let error = gateway.close_session(&session).await;

        assert!(matches!(error, Err(SessionGatewayError::Backend(_))));
        assert_eq!(gateway.registry().len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_should_reject_missing_session_before_lock() -> anyhow::Result<()> {
        let mut gateway = SessionGateway::new(FakeBackend::default(), Duration::from_secs(30));
        let error =
            gateway.acquire_controller(&SessionName::new("missing")?, browser(1)?, Instant::now());

        assert!(matches!(
            error,
            Err(SessionGatewayError::Registry(
                SessionRegistryError::NotRegistered { .. }
            ))
        ));
        Ok(())
    }
}
