//! Backend session adapter contracts.
//!
//! Backends such as rmux or tmux own the real session, pane, screen state, and
//! native local attach path. `termstage` reaches those backends through this
//! adapter boundary instead of owning a second local command PTY.

use bytes::Bytes;
use thiserror::Error;

use crate::protocol::{SafeMessage, SessionName, TerminalSize};

/// Terminal backend implementation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    /// rmux backend.
    Rmux,
    /// tmux backend.
    Tmux,
}

/// Backend window identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendWindowId(SafeMessage);

impl BackendWindowId {
    /// Creates a backend window id.
    ///
    /// # Errors
    ///
    /// Returns an error when the id is too long for safe control messages.
    pub fn new(value: impl Into<String>) -> Result<Self, BackendError> {
        Ok(Self(SafeMessage::new(value)?))
    }

    /// Returns the window id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Backend pane identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendPaneId(SafeMessage);

impl BackendPaneId {
    /// Creates a backend pane id.
    ///
    /// # Errors
    ///
    /// Returns an error when the id is too long for safe control messages.
    pub fn new(value: impl Into<String>) -> Result<Self, BackendError> {
        Ok(Self(SafeMessage::new(value)?))
    }

    /// Returns the pane id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Reference to a concrete backend session/window/pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSessionRef {
    kind: BackendKind,
    session: SessionName,
    window: BackendWindowId,
    pane: BackendPaneId,
}

impl BackendSessionRef {
    /// Creates a backend session reference.
    #[must_use]
    pub const fn new(
        kind: BackendKind,
        session: SessionName,
        window: BackendWindowId,
        pane: BackendPaneId,
    ) -> Self {
        Self {
            kind,
            session,
            window,
            pane,
        }
    }

    /// Returns the backend kind.
    #[must_use]
    pub const fn kind(&self) -> BackendKind {
        self.kind
    }

    /// Returns the backend session name.
    #[must_use]
    pub const fn session(&self) -> &SessionName {
        &self.session
    }

    /// Returns the backend window id.
    #[must_use]
    pub const fn window(&self) -> &BackendWindowId {
        &self.window
    }

    /// Returns the backend pane id.
    #[must_use]
    pub const fn pane(&self) -> &BackendPaneId {
        &self.pane
    }
}

/// Snapshot of a backend pane screen for semantic API responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendScreenSnapshot {
    size: TerminalSize,
    cursor_col: u16,
    cursor_row: u16,
    lines: Vec<String>,
}

impl BackendScreenSnapshot {
    /// Creates a backend screen snapshot.
    #[must_use]
    pub fn new(size: TerminalSize, cursor_col: u16, cursor_row: u16, lines: Vec<String>) -> Self {
        Self {
            size,
            cursor_col,
            cursor_row,
            lines,
        }
    }

    /// Returns the screen size.
    #[must_use]
    pub const fn size(&self) -> TerminalSize {
        self.size
    }

    /// Returns the cursor column.
    #[must_use]
    pub const fn cursor_col(&self) -> u16 {
        self.cursor_col
    }

    /// Returns the cursor row.
    #[must_use]
    pub const fn cursor_row(&self) -> u16 {
        self.cursor_row
    }

    /// Returns screen lines.
    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
}

/// Event emitted by a backend pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendEvent {
    /// Backend emitted VT/ANSI output bytes.
    Output {
        /// Output bytes.
        bytes: Bytes,
    },
    /// Backend pane resized.
    Resized {
        /// New pane size.
        size: TerminalSize,
    },
    /// Backend session closed.
    Closed {
        /// Safe close message.
        message: SafeMessage,
    },
}

/// Backend adapter failure.
#[derive(Debug, Error)]
pub enum BackendError {
    /// Protocol validation failed.
    #[error("invalid backend value")]
    Protocol(#[from] crate::protocol::ProtocolError),
    /// The backend executable or service is unavailable.
    #[error("backend is unavailable")]
    Unavailable,
    /// The requested session was not found.
    #[error("backend session was not found")]
    SessionNotFound,
    /// Backend IO failed.
    #[error("backend io failed")]
    Io(#[source] std::io::Error),
    /// Backend operation failed with a safe message.
    #[error("backend operation failed: {0:?}")]
    Operation(SafeMessage),
}

// Native async trait methods keep adapter implementations readable and match
// AGENTS.md guidance. This trait is crate-owned and not used for dyn dispatch.
#[allow(async_fn_in_trait)]
/// Adapter boundary for backend-owned terminal sessions.
pub trait BackendAdapter: Send {
    /// Creates or finds a backend session and returns its active pane reference.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend cannot create or resolve the
    /// session.
    async fn create_or_find_session(
        &mut self,
        session: &SessionName,
        size: TerminalSize,
    ) -> Result<BackendSessionRef, BackendError>;

    /// Writes terminal input bytes to a backend pane.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend rejects or cannot write input.
    async fn write_input(
        &mut self,
        target: &BackendSessionRef,
        bytes: Bytes,
    ) -> Result<(), BackendError>;

    /// Resizes a backend pane.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend cannot resize the pane.
    async fn resize(
        &mut self,
        target: &BackendSessionRef,
        size: TerminalSize,
    ) -> Result<(), BackendError>;

    /// Reads a screen snapshot from a backend pane.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend cannot provide screen state.
    async fn read_screen(
        &mut self,
        target: &BackendSessionRef,
    ) -> Result<BackendScreenSnapshot, BackendError>;

    /// Closes or detaches a backend session according to caller policy.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend cannot close or detach.
    async fn close_session(&mut self, target: &BackendSessionRef) -> Result<(), BackendError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_create_backend_session_reference() -> anyhow::Result<()> {
        let reference = BackendSessionRef::new(
            BackendKind::Tmux,
            SessionName::new("demo")?,
            BackendWindowId::new("0")?,
            BackendPaneId::new("%1")?,
        );

        assert_eq!(reference.kind(), BackendKind::Tmux);
        assert_eq!(reference.session().as_str(), "demo");
        assert_eq!(reference.window().as_str(), "0");
        assert_eq!(reference.pane().as_str(), "%1");
        Ok(())
    }

    #[test]
    fn test_should_create_screen_snapshot() -> anyhow::Result<()> {
        let snapshot =
            BackendScreenSnapshot::new(TerminalSize::new(80, 24)?, 4, 3, vec!["prompt".to_owned()]);

        assert_eq!(snapshot.size(), TerminalSize::new(80, 24)?);
        assert_eq!(snapshot.cursor_col(), 4);
        assert_eq!(snapshot.cursor_row(), 3);
        assert_eq!(snapshot.lines(), ["prompt"]);
        Ok(())
    }
}
