//! Backend session adapter contracts.
//!
//! Backends such as rmux or tmux own the real session, pane, screen state, and
//! native local attach path. `termstage` reaches those backends through this
//! adapter boundary instead of owning a second local command PTY.

use bytes::Bytes;
use thiserror::Error;

use crate::protocol::{SafeMessage, SessionName, TerminalSize};

/// Backend scroll direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendScrollDirection {
    /// Scroll up through backend history.
    Up,
    /// Scroll down through backend history.
    Down,
}

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

/// Result of creating or finding a backend session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSessionResolution {
    reference: BackendSessionRef,
    created: bool,
}

impl BackendSessionResolution {
    /// Creates a backend session resolution.
    #[must_use]
    pub const fn new(reference: BackendSessionRef, created: bool) -> Self {
        Self { reference, created }
    }

    /// Returns the resolved backend reference.
    #[must_use]
    pub const fn reference(&self) -> &BackendSessionRef {
        &self.reference
    }

    /// Returns whether the backend session was created by this operation.
    #[must_use]
    pub const fn created(&self) -> bool {
        self.created
    }

    /// Consumes the resolution and returns the backend reference.
    #[must_use]
    pub fn into_reference(self) -> BackendSessionRef {
        self.reference
    }
}

/// Snapshot of a backend pane screen for semantic API responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendScreenSnapshot {
    size: TerminalSize,
    cursor_col: u16,
    cursor_row: u16,
    cursor_visible: bool,
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
            cursor_visible: true,
            lines,
        }
    }

    /// Creates a backend screen snapshot with explicit cursor visibility.
    #[must_use]
    pub fn new_with_cursor_visibility(
        size: TerminalSize,
        cursor_col: u16,
        cursor_row: u16,
        cursor_visible: bool,
        lines: Vec<String>,
    ) -> Self {
        Self {
            size,
            cursor_col,
            cursor_row,
            cursor_visible,
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

    /// Returns whether the backend cursor is visible.
    #[must_use]
    pub const fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Returns screen lines.
    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
}

/// Snapshot of a backend pane screen for terminal replay.
///
/// Unlike [`BackendScreenSnapshot`], this type is allowed to carry terminal
/// escape sequences in `lines` so a browser xterm can reproduce colors and
/// other display attributes. Semantic API responses should continue to use
/// [`BackendScreenSnapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendTerminalSnapshot {
    size: TerminalSize,
    cursor_col: u16,
    cursor_row: u16,
    cursor_visible: bool,
    lines: Vec<String>,
}

impl BackendTerminalSnapshot {
    /// Creates a backend terminal snapshot.
    #[must_use]
    pub fn new(size: TerminalSize, cursor_col: u16, cursor_row: u16, lines: Vec<String>) -> Self {
        Self {
            size,
            cursor_col,
            cursor_row,
            cursor_visible: true,
            lines,
        }
    }

    /// Creates a backend terminal snapshot with explicit cursor visibility.
    #[must_use]
    pub fn new_with_cursor_visibility(
        size: TerminalSize,
        cursor_col: u16,
        cursor_row: u16,
        cursor_visible: bool,
        lines: Vec<String>,
    ) -> Self {
        Self {
            size,
            cursor_col,
            cursor_row,
            cursor_visible,
            lines,
        }
    }

    /// Converts a semantic snapshot into a plain terminal replay snapshot.
    #[must_use]
    pub fn from_screen(snapshot: BackendScreenSnapshot) -> Self {
        Self {
            size: snapshot.size,
            cursor_col: snapshot.cursor_col,
            cursor_row: snapshot.cursor_row,
            cursor_visible: snapshot.cursor_visible,
            lines: snapshot.lines,
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

    /// Returns whether the backend cursor is visible.
    #[must_use]
    pub const fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Returns terminal replay lines.
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
    /// Backend output was not valid UTF-8.
    #[error("backend output was not valid utf-8")]
    Utf8(#[source] std::string::FromUtf8Error),
    /// Terminal input bytes could not be represented for this backend.
    #[error("backend input bytes are unsupported")]
    UnsupportedInput,
    /// Backend operation failed with a safe message.
    #[error("backend operation failed: {0:?}")]
    Operation(SafeMessage),
}

// Native async trait methods keep adapter implementations readable and match
// AGENTS.md guidance. This trait is crate-owned and not used for dyn dispatch.
#[allow(async_fn_in_trait)]
/// Adapter boundary for backend-owned terminal sessions.
pub trait BackendAdapter: Send {
    /// Creates or finds a backend session and returns its active pane reference
    /// plus whether a new backend session was created.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend cannot create or resolve the
    /// session.
    async fn create_or_find_session(
        &mut self,
        session: &SessionName,
        size: TerminalSize,
    ) -> Result<BackendSessionResolution, BackendError>;

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

    /// Sends literal text to a backend pane.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend rejects or cannot write text.
    async fn send_text(
        &mut self,
        target: &BackendSessionRef,
        text: &str,
    ) -> Result<(), BackendError>;

    /// Sends one backend-compatible key token to a pane.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend rejects or cannot send the key.
    async fn send_key(&mut self, target: &BackendSessionRef, key: &str)
    -> Result<(), BackendError>;

    /// Sends a command and confirms it with Enter.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend cannot submit the command.
    async fn run_command(
        &mut self,
        target: &BackendSessionRef,
        command: &str,
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

    /// Reads a terminal replay snapshot from a backend pane.
    ///
    /// Implementations should preserve display attributes when the backend can
    /// provide them. The default implementation falls back to the plain
    /// semantic screen snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend cannot provide screen state.
    async fn read_terminal_screen(
        &mut self,
        target: &BackendSessionRef,
    ) -> Result<BackendTerminalSnapshot, BackendError> {
        self.read_screen(target)
            .await
            .map(BackendTerminalSnapshot::from_screen)
    }

    /// Reports whether a backend-native local client is attached.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend cannot inspect native client
    /// state.
    async fn has_native_client(
        &mut self,
        _target: &BackendSessionRef,
    ) -> Result<bool, BackendError> {
        Ok(false)
    }

    /// Scrolls backend-visible pane history.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend cannot scroll the pane.
    async fn scroll(
        &mut self,
        target: &BackendSessionRef,
        direction: BackendScrollDirection,
        amount: u16,
    ) -> Result<(), BackendError>;

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
    fn test_should_create_backend_session_resolution() -> anyhow::Result<()> {
        let reference = BackendSessionRef::new(
            BackendKind::Tmux,
            SessionName::new("demo")?,
            BackendWindowId::new("0")?,
            BackendPaneId::new("%1")?,
        );
        let resolution = BackendSessionResolution::new(reference.clone(), true);

        assert!(resolution.created());
        assert_eq!(resolution.reference(), &reference);
        assert_eq!(resolution.into_reference(), reference);
        Ok(())
    }

    #[test]
    fn test_should_create_screen_snapshot() -> anyhow::Result<()> {
        let snapshot =
            BackendScreenSnapshot::new(TerminalSize::new(80, 24)?, 4, 3, vec!["prompt".to_owned()]);

        assert_eq!(snapshot.size(), TerminalSize::new(80, 24)?);
        assert_eq!(snapshot.cursor_col(), 4);
        assert_eq!(snapshot.cursor_row(), 3);
        assert!(snapshot.cursor_visible());
        assert_eq!(snapshot.lines(), ["prompt"]);

        let hidden_cursor = BackendScreenSnapshot::new_with_cursor_visibility(
            TerminalSize::new(80, 24)?,
            4,
            3,
            false,
            vec!["prompt".to_owned()],
        );
        assert!(!hidden_cursor.cursor_visible());
        Ok(())
    }
}
