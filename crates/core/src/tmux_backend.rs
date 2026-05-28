//! tmux backend adapter.
//!
//! This adapter is the first backend-owned session implementation. It uses tmux
//! commands to create/find a session, resolve the active pane, write input,
//! resize the pane, read the visible screen, and close the session.

use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};

use bytes::Bytes;
use tokio::process::Command;

use crate::{
    backend::{
        BackendAdapter, BackendError, BackendKind, BackendPaneId, BackendScreenSnapshot,
        BackendScrollDirection, BackendSessionRef, BackendSessionResolution, BackendWindowId,
    },
    protocol::{SafeMessage, SessionName, TerminalSize},
};

const TERMINAL_COLOR_MODE: &str = "truecolor";
const TERMINAL_PROGRAM: &str = "termstage";
const TMUX_HISTORY_LIMIT: &str = "100000";
const DISABLE_COLOR_ENV: [&str; 2] = ["NO_COLOR", "ANSI_COLORS_DISABLED"];

/// tmux backend adapter.
#[derive(Debug, Clone)]
pub struct TmuxBackend {
    tmux: PathBuf,
}

/// Command used to start a new tmux session pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSessionCommand {
    executable: OsString,
    args: Vec<OsString>,
}

impl TmuxSessionCommand {
    /// Creates a tmux session command.
    #[must_use]
    pub fn new(executable: OsString, args: Vec<OsString>) -> Self {
        Self { executable, args }
    }

    /// Returns the executable.
    #[must_use]
    pub fn executable(&self) -> &OsString {
        &self.executable
    }

    /// Returns the command arguments.
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }
}

/// tmux session details used by supervisor CLI commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSessionInfo {
    session: SessionName,
    window: BackendWindowId,
    pane: BackendPaneId,
    size: TerminalSize,
}

impl TmuxSessionInfo {
    /// Creates tmux session details.
    #[must_use]
    pub const fn new(
        session: SessionName,
        window: BackendWindowId,
        pane: BackendPaneId,
        size: TerminalSize,
    ) -> Self {
        Self {
            session,
            window,
            pane,
            size,
        }
    }

    /// Returns the session name.
    #[must_use]
    pub const fn session(&self) -> &SessionName {
        &self.session
    }

    /// Returns the active window id.
    #[must_use]
    pub const fn window(&self) -> &BackendWindowId {
        &self.window
    }

    /// Returns the active pane id.
    #[must_use]
    pub const fn pane(&self) -> &BackendPaneId {
        &self.pane
    }

    /// Returns the active pane size.
    #[must_use]
    pub const fn size(&self) -> TerminalSize {
        self.size
    }
}

impl TmuxBackend {
    /// Resolves `tmux` from `PATH`.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::Unavailable`] when the executable cannot be
    /// found.
    pub fn from_path() -> Result<Self, BackendError> {
        let tmux = which::which("tmux").map_err(|_error| BackendError::Unavailable)?;
        Ok(Self { tmux })
    }

    /// Creates a tmux backend adapter from a known executable path.
    #[must_use]
    pub fn new(tmux: PathBuf) -> Self {
        Self { tmux }
    }

    /// Returns the configured tmux path.
    #[must_use]
    pub fn tmux_path(&self) -> &Path {
        &self.tmux
    }

    /// Lists tmux sessions whose names are valid termstage session names.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when tmux cannot be invoked or reports an
    /// unexpected failure.
    pub async fn list_sessions(&self) -> Result<Vec<SessionName>, BackendError> {
        let output = self
            .command()
            .args(["list-sessions", "-F", "#{session_name}"])
            .output()
            .await
            .map_err(BackendError::Io)?;
        if !output.status.success() {
            let stderr = String::from_utf8(output.stderr).map_err(BackendError::Utf8)?;
            if stderr.contains("no server running") {
                return Ok(Vec::new());
            }
            return Err(BackendError::Operation(safe_trimmed_message(
                &stderr,
                "tmux list-sessions failed",
            )?));
        }
        let text = String::from_utf8(output.stdout).map_err(BackendError::Utf8)?;
        let sessions = text
            .lines()
            .filter_map(|line| SessionName::new(line.trim()).ok())
            .collect();
        Ok(sessions)
    }

    /// Reads details for a tmux session.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the session cannot be resolved.
    pub async fn inspect_session(
        &self,
        session: &SessionName,
    ) -> Result<TmuxSessionInfo, BackendError> {
        let output = self
            .command()
            .args([
                "display-message",
                "-p",
                "-t",
                session.as_str(),
                "#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_width}\t#{pane_height}",
            ])
            .output()
            .await
            .map_err(BackendError::Io)?;
        let text = Self::success_stdout(output, "tmux display-message failed")?;
        parse_session_info(&text)
    }

    /// Kills a tmux session by name.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when tmux cannot kill the session.
    pub async fn kill_session_by_name(&self, session: &SessionName) -> Result<(), BackendError> {
        self.run(["kill-session", "-t", session.as_str()]).await
    }

    /// Returns whether a tmux session exists.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when tmux cannot check the session.
    pub async fn session_exists_by_name(
        &self,
        session: &SessionName,
    ) -> Result<bool, BackendError> {
        self.session_exists(session).await
    }

    /// Creates a new tmux session and starts `command` as the first pane
    /// command. When `command` is `None`, tmux starts the user's default shell.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the session already exists or tmux cannot
    /// create or inspect the session.
    pub async fn create_new_session(
        &self,
        session: &SessionName,
        size: TerminalSize,
        command: Option<&TmuxSessionCommand>,
    ) -> Result<TmuxSessionInfo, BackendError> {
        if self.session_exists(session).await? {
            return Err(BackendError::Operation(SafeMessage::from_static(
                "tmux session already exists",
            )));
        }
        self.create_session(session, size, command).await?;
        self.prepare_session(session).await?;
        self.inspect_session(session).await
    }

    /// Resolves and prepares an existing tmux session for gateway attachment.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::SessionNotFound`] when the session does not
    /// exist, or another [`BackendError`] when tmux cannot inspect it.
    pub async fn attach_existing_session(
        &self,
        session: &SessionName,
    ) -> Result<BackendSessionRef, BackendError> {
        if !self.session_exists(session).await? {
            return Err(BackendError::SessionNotFound);
        }
        self.prepare_session(session).await?;
        self.resolve_reference(session).await
    }

    async fn ensure_session(
        &self,
        session: &SessionName,
        size: TerminalSize,
    ) -> Result<bool, BackendError> {
        if self.session_exists(session).await? {
            return Ok(false);
        }
        self.create_session(session, size, None).await?;
        Ok(true)
    }

    async fn session_exists(&self, session: &SessionName) -> Result<bool, BackendError> {
        let output = self
            .command()
            .arg("has-session")
            .arg("-t")
            .arg(session.as_str())
            .output()
            .await
            .map_err(BackendError::Io)?;
        Ok(output.status.success())
    }

    async fn create_session(
        &self,
        session: &SessionName,
        size: TerminalSize,
        command: Option<&TmuxSessionCommand>,
    ) -> Result<(), BackendError> {
        let cols = size.cols.get().to_string();
        let rows = size.rows.get().to_string();
        let default_command;
        let command = if let Some(command) = command {
            command
        } else {
            default_command = TmuxSessionCommand::new(default_shell_command(), Vec::new());
            &default_command
        };
        let mut process = self.command();
        process
            .env("COLORTERM", TERMINAL_COLOR_MODE)
            .env("CLICOLOR", "1")
            .env("TERM_PROGRAM", TERMINAL_PROGRAM)
            .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"))
            .env_remove("NO_COLOR")
            .env_remove("ANSI_COLORS_DISABLED")
            .arg("new-session")
            .arg("-d")
            .arg("-s")
            .arg(session.as_str())
            .arg("-x")
            .arg(&cols)
            .arg("-y")
            .arg(&rows)
            .arg("-e")
            .arg(format!("COLORTERM={TERMINAL_COLOR_MODE}"))
            .arg("-e")
            .arg("CLICOLOR=1")
            .arg("-e")
            .arg(format!("TERM_PROGRAM={TERMINAL_PROGRAM}"))
            .arg("-e")
            .arg(format!(
                "TERM_PROGRAM_VERSION={}",
                env!("CARGO_PKG_VERSION")
            ))
            .arg("--")
            .arg(command.executable())
            .args(command.args());
        let output = process.output().await.map_err(BackendError::Io)?;
        Self::ensure_success(output, "tmux new-session failed")
    }

    async fn prepare_session(&self, session: &SessionName) -> Result<(), BackendError> {
        for name in DISABLE_COLOR_ENV {
            self.run(["set-environment", "-t", session.as_str(), "-u", name])
                .await?;
        }
        self.run([
            "set-environment",
            "-t",
            session.as_str(),
            "COLORTERM",
            TERMINAL_COLOR_MODE,
        ])
        .await?;
        self.run(["set-environment", "-t", session.as_str(), "CLICOLOR", "1"])
            .await?;
        self.run([
            "set-environment",
            "-t",
            session.as_str(),
            "TERM_PROGRAM",
            TERMINAL_PROGRAM,
        ])
        .await?;
        self.run([
            "set-environment",
            "-t",
            session.as_str(),
            "TERM_PROGRAM_VERSION",
            env!("CARGO_PKG_VERSION"),
        ])
        .await?;
        self.run(["set-option", "-t", session.as_str(), "mouse", "off"])
            .await?;
        self.run([
            "set-option",
            "-t",
            session.as_str(),
            "history-limit",
            TMUX_HISTORY_LIMIT,
        ])
        .await
    }

    async fn has_attached_client(&self, session: &SessionName) -> Result<bool, BackendError> {
        let output = self
            .command()
            .args([
                "display-message",
                "-p",
                "-t",
                session.as_str(),
                "#{session_attached}",
            ])
            .output()
            .await
            .map_err(BackendError::Io)?;
        let text = Self::success_stdout(output, "tmux display-message failed")?;
        Ok(parse_u16(text.trim())? > 0)
    }

    async fn set_window_size_latest(&self, target: &BackendSessionRef) -> Result<(), BackendError> {
        self.run([
            "set-window-option",
            "-t",
            target.window().as_str(),
            "window-size",
            "latest",
        ])
        .await
    }

    async fn resolve_reference(
        &self,
        session: &SessionName,
    ) -> Result<BackendSessionRef, BackendError> {
        let output = self
            .command()
            .args([
                "display-message",
                "-p",
                "-t",
                session.as_str(),
                "#{window_id} #{pane_id}",
            ])
            .output()
            .await
            .map_err(BackendError::Io)?;
        let text = Self::success_stdout(output, "tmux display-message failed")?;
        let mut parts = text.split_ascii_whitespace();
        let Some(window) = parts.next() else {
            return Err(BackendError::Operation(SafeMessage::from_static(
                "tmux did not report window id",
            )));
        };
        let Some(pane) = parts.next() else {
            return Err(BackendError::Operation(SafeMessage::from_static(
                "tmux did not report pane id",
            )));
        };
        if parts.next().is_some() {
            return Err(BackendError::Operation(SafeMessage::from_static(
                "tmux reported unexpected pane identity",
            )));
        }
        Ok(BackendSessionRef::new(
            BackendKind::Tmux,
            session.clone(),
            BackendWindowId::new(window)?,
            BackendPaneId::new(pane)?,
        ))
    }

    async fn run<const N: usize>(&self, args: [&str; N]) -> Result<(), BackendError> {
        let output = self
            .command()
            .args(args)
            .output()
            .await
            .map_err(BackendError::Io)?;
        Self::ensure_success(output, "tmux command failed")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.tmux);
        command.env_remove("TMUX");
        command
    }

    fn ensure_success(
        output: std::process::Output,
        fallback: &'static str,
    ) -> Result<(), BackendError> {
        if output.status.success() {
            Ok(())
        } else {
            Err(BackendError::Operation(output_message(output, fallback)?))
        }
    }

    fn success_stdout(
        output: std::process::Output,
        fallback: &'static str,
    ) -> Result<String, BackendError> {
        if !output.status.success() {
            return Err(BackendError::Operation(output_message(output, fallback)?));
        }
        String::from_utf8(output.stdout).map_err(BackendError::Utf8)
    }
}

impl BackendAdapter for TmuxBackend {
    async fn create_or_find_session(
        &mut self,
        session: &SessionName,
        size: TerminalSize,
    ) -> Result<BackendSessionResolution, BackendError> {
        let created = self.ensure_session(session, size).await?;
        self.prepare_session(session).await?;
        let reference = self.resolve_reference(session).await?;
        if created {
            self.set_window_size_latest(&reference).await?;
        } else {
            self.resize(&reference, size).await?;
        }
        Ok(BackendSessionResolution::new(reference, created))
    }

    async fn write_input(
        &mut self,
        target: &BackendSessionRef,
        bytes: Bytes,
    ) -> Result<(), BackendError> {
        let input =
            String::from_utf8(bytes.to_vec()).map_err(|_error| BackendError::UnsupportedInput)?;
        self.send_text(target, &input).await
    }

    async fn send_text(
        &mut self,
        target: &BackendSessionRef,
        text: &str,
    ) -> Result<(), BackendError> {
        self.run(["send-keys", "-t", target.pane().as_str(), "-l", "--", text])
            .await
    }

    async fn send_key(
        &mut self,
        target: &BackendSessionRef,
        key: &str,
    ) -> Result<(), BackendError> {
        if key == "Enter" {
            return self
                .run(["send-keys", "-t", target.pane().as_str(), "C-j"])
                .await;
        }
        if key.starts_with('-') {
            self.run(["send-keys", "-t", target.pane().as_str(), "--", key])
                .await
        } else {
            self.run(["send-keys", "-t", target.pane().as_str(), key])
                .await
        }
    }

    async fn run_command(
        &mut self,
        target: &BackendSessionRef,
        command: &str,
    ) -> Result<(), BackendError> {
        self.run(["send-keys", "-t", target.pane().as_str(), command, "Enter"])
            .await
    }

    async fn resize(
        &mut self,
        target: &BackendSessionRef,
        size: TerminalSize,
    ) -> Result<(), BackendError> {
        let cols = size.cols.get().to_string();
        let rows = size.rows.get().to_string();
        self.run([
            "resize-window",
            "-t",
            target.session().as_str(),
            "-x",
            &cols,
            "-y",
            &rows,
        ])
        .await?;
        self.run([
            "resize-pane",
            "-t",
            target.pane().as_str(),
            "-x",
            &cols,
            "-y",
            &rows,
        ])
        .await?;
        self.set_window_size_latest(target).await
    }

    async fn read_screen(
        &mut self,
        target: &BackendSessionRef,
    ) -> Result<BackendScreenSnapshot, BackendError> {
        let metadata = self
            .command()
            .args([
                "display-message",
                "-p",
                "-t",
                target.pane().as_str(),
                "#{pane_width} #{pane_height} #{cursor_x} #{cursor_y} #{cursor_flag}",
            ])
            .output()
            .await
            .map_err(BackendError::Io)?;
        let metadata = Self::success_stdout(metadata, "tmux display-message failed")?;
        let mut parts = metadata.split_ascii_whitespace();
        let Some(cols) = parts.next() else {
            return Err(BackendError::Operation(SafeMessage::from_static(
                "tmux did not report pane width",
            )));
        };
        let Some(rows) = parts.next() else {
            return Err(BackendError::Operation(SafeMessage::from_static(
                "tmux did not report pane height",
            )));
        };
        let Some(cursor_col) = parts.next() else {
            return Err(BackendError::Operation(SafeMessage::from_static(
                "tmux did not report cursor column",
            )));
        };
        let Some(cursor_row) = parts.next() else {
            return Err(BackendError::Operation(SafeMessage::from_static(
                "tmux did not report cursor row",
            )));
        };

        let size = TerminalSize::new(parse_u16(cols)?, parse_u16(rows)?)?;
        let cursor_col = parse_u16(cursor_col)?;
        let cursor_row = parse_u16(cursor_row)?;
        let cursor_visible = match parts.next() {
            Some(cursor_flag) => parse_u16(cursor_flag)? > 0,
            None => true,
        };
        let output = self
            .command()
            .args(["capture-pane", "-e", "-p", "-t", target.pane().as_str()])
            .output()
            .await
            .map_err(BackendError::Io)?;
        let text = Self::success_stdout(output, "tmux capture-pane failed")?;
        let lines = text.lines().map(ToOwned::to_owned).collect();
        Ok(BackendScreenSnapshot::new_with_cursor_visibility(
            size,
            cursor_col,
            cursor_row,
            cursor_visible,
            lines,
        ))
    }

    async fn has_native_client(
        &mut self,
        target: &BackendSessionRef,
    ) -> Result<bool, BackendError> {
        self.has_attached_client(target.session()).await
    }

    async fn scroll(
        &mut self,
        target: &BackendSessionRef,
        direction: BackendScrollDirection,
        amount: u16,
    ) -> Result<(), BackendError> {
        let amount = amount.to_string();
        let command = match direction {
            BackendScrollDirection::Up => "scroll-up",
            BackendScrollDirection::Down => "scroll-down",
        };
        self.run(["copy-mode", "-t", target.pane().as_str()])
            .await?;
        self.run([
            "send-keys",
            "-t",
            target.pane().as_str(),
            "-X",
            "-N",
            &amount,
            command,
        ])
        .await
    }

    async fn close_session(&mut self, target: &BackendSessionRef) -> Result<(), BackendError> {
        self.run(["kill-session", "-t", target.session().as_str()])
            .await
    }
}

fn parse_u16(value: &str) -> Result<u16, BackendError> {
    value.parse::<u16>().map_err(|_error| {
        BackendError::Operation(SafeMessage::from_static(
            "tmux reported invalid numeric value",
        ))
    })
}

fn default_shell_command() -> OsString {
    env::var_os("SHELL")
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| OsString::from("/bin/sh"))
}

fn parse_session_info(text: &str) -> Result<TmuxSessionInfo, BackendError> {
    let mut parts = text.trim_end().split('\t');
    let Some(session) = parts.next() else {
        return Err(BackendError::Operation(SafeMessage::from_static(
            "tmux did not report session name",
        )));
    };
    let Some(window) = parts.next() else {
        return Err(BackendError::Operation(SafeMessage::from_static(
            "tmux did not report window id",
        )));
    };
    let Some(pane) = parts.next() else {
        return Err(BackendError::Operation(SafeMessage::from_static(
            "tmux did not report pane id",
        )));
    };
    let Some(cols) = parts.next() else {
        return Err(BackendError::Operation(SafeMessage::from_static(
            "tmux did not report pane width",
        )));
    };
    let Some(rows) = parts.next() else {
        return Err(BackendError::Operation(SafeMessage::from_static(
            "tmux did not report pane height",
        )));
    };
    if parts.next().is_some() {
        return Err(BackendError::Operation(SafeMessage::from_static(
            "tmux reported unexpected session details",
        )));
    }
    Ok(TmuxSessionInfo::new(
        SessionName::new(session.to_owned())?,
        BackendWindowId::new(window)?,
        BackendPaneId::new(pane)?,
        TerminalSize::new(parse_u16(cols)?, parse_u16(rows)?)?,
    ))
}

fn output_message(
    output: std::process::Output,
    fallback: &'static str,
) -> Result<SafeMessage, BackendError> {
    let stderr = String::from_utf8(output.stderr).map_err(BackendError::Utf8)?;
    safe_trimmed_message(&stderr, fallback)
}

fn safe_trimmed_message(text: &str, fallback: &'static str) -> Result<SafeMessage, BackendError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        Ok(SafeMessage::from_static(fallback))
    } else {
        SafeMessage::new(trimmed).map_err(BackendError::Protocol)
    }
}

#[cfg(test)]
mod tests {
    use std::{process::Stdio, time::Duration};

    use anyhow::Context;
    use tokio::{process::Command as TokioCommand, time::sleep};

    use super::*;

    #[derive(Debug)]
    struct TmuxSessionCleanup {
        tmux: PathBuf,
        session: SessionName,
        active: bool,
    }

    impl TmuxSessionCleanup {
        fn new(tmux: PathBuf, session: SessionName) -> Self {
            Self {
                tmux,
                session,
                active: true,
            }
        }

        fn disarm(&mut self) {
            self.active = false;
        }
    }

    impl Drop for TmuxSessionCleanup {
        #[allow(
            clippy::disallowed_types,
            reason = "test cleanup runs from Drop and cannot await tokio::process::Command"
        )]
        fn drop(&mut self) {
            if !self.active {
                return;
            }
            let _result = std::process::Command::new(&self.tmux)
                .env_remove("TMUX")
                .args(["kill-session", "-t", self.session.as_str()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    #[tokio::test]
    async fn test_should_create_write_read_resize_and_close_tmux_session() -> anyhow::Result<()> {
        let tmux = which::which("tmux").context("tmux unavailable")?;
        let session = SessionName::new(format!("termstage-backend-test-{}", std::process::id()))?;
        let mut cleanup = TmuxSessionCleanup::new(tmux.clone(), session.clone());
        let mut backend = TmuxBackend::new(tmux.clone());
        let resolution = backend
            .create_or_find_session(&session, TerminalSize::new(80, 24)?)
            .await?;
        assert!(resolution.created());
        let reference = resolution.into_reference();

        assert_eq!(reference.kind(), BackendKind::Tmux);
        assert_eq!(reference.session(), &session);
        assert!(!reference.window().as_str().is_empty());
        assert!(!reference.pane().as_str().is_empty());
        assert_eq!(session_option(&tmux, &session, "mouse").await?, "off");

        backend
            .run_command(&reference, "printf termstage-backend-ok")
            .await?;
        sleep(Duration::from_millis(300)).await;
        let snapshot = backend.read_screen(&reference).await?;
        assert!(
            snapshot
                .lines()
                .iter()
                .any(|line| line.contains("termstage-backend-ok"))
        );

        backend.send_text(&reference, "printf text-ok").await?;
        backend.send_key(&reference, "Enter").await?;
        sleep(Duration::from_millis(300)).await;
        let snapshot = backend.read_screen(&reference).await?;
        assert!(snapshot.lines().iter().any(|line| line.contains("text-ok")));

        backend
            .resize(&reference, TerminalSize::new(100, 30)?)
            .await?;
        let resized = backend.read_screen(&reference).await?;
        assert_eq!(resized.size(), TerminalSize::new(100, 30)?);
        assert_eq!(
            window_size_option(&tmux, reference.window()).await?,
            "latest"
        );

        backend.close_session(&reference).await?;
        cleanup.disarm();
        let status = TokioCommand::new(tmux)
            .env_remove("TMUX")
            .args(["has-session", "-t", session.as_str()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
        assert!(!status.success());
        Ok(())
    }

    #[tokio::test]
    async fn test_should_preserve_color_attributes_when_reading_tmux_screen() -> anyhow::Result<()>
    {
        let tmux = which::which("tmux").context("tmux unavailable")?;
        let session = SessionName::new(format!("termstage-backend-color-{}", std::process::id()))?;
        let _cleanup = TmuxSessionCleanup::new(tmux.clone(), session.clone());
        let status = TokioCommand::new(&tmux)
            .env_remove("TMUX")
            .args([
                "new-session",
                "-d",
                "-s",
                session.as_str(),
                "printf '\\033[31mtermstage-red\\033[0m\\n'; sleep 2",
            ])
            .status()
            .await
            .context("failed to create tmux color test session")?;
        if !status.success() {
            anyhow::bail!("tmux color test session exited with {status}");
        }
        let mut backend = TmuxBackend::new(tmux);
        let resolution = backend
            .create_or_find_session(&session, TerminalSize::new(80, 24)?)
            .await?;
        assert!(!resolution.created());
        let reference = resolution.into_reference();
        sleep(Duration::from_millis(300)).await;
        let snapshot = backend.read_screen(&reference).await?;

        assert!(
            snapshot
                .lines()
                .iter()
                .any(|line| line.contains("termstage-red") && line.contains("\x1b[")),
            "tmux capture did not preserve color attributes: {:?}",
            snapshot.lines()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_should_detect_attached_tmux_client() -> anyhow::Result<()> {
        let tmux = which::which("tmux").context("tmux unavailable")?;
        let session = SessionName::new(format!("termstage-backend-attach-{}", std::process::id()))?;
        let mut cleanup = TmuxSessionCleanup::new(tmux.clone(), session.clone());
        let mut backend = TmuxBackend::new(tmux.clone());
        let reference = backend
            .create_or_find_session(&session, TerminalSize::new(80, 24)?)
            .await?
            .into_reference();
        assert!(!backend.has_native_client(&reference).await?);
        let mut client = TokioCommand::new(&tmux)
            .env_remove("TMUX")
            .args(["-C", "attach-session", "-t", session.as_str()])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn tmux control client")?;
        sleep(Duration::from_millis(200)).await;

        assert!(backend.has_native_client(&reference).await?);

        let _kill_result = client.kill().await;
        let _status = client.wait().await.context("tmux control client failed")?;
        backend.close_session(&reference).await?;
        cleanup.disarm();
        Ok(())
    }

    #[tokio::test]
    async fn test_should_report_missing_tmux_backend() -> anyhow::Result<()> {
        let mut backend = TmuxBackend::new(PathBuf::from("/definitely/missing/tmux"));
        let result = backend
            .create_or_find_session(
                &SessionName::new("missing-tmux")?,
                TerminalSize::new(80, 24)?,
            )
            .await;

        assert!(matches!(result, Err(BackendError::Io(_))));
        Ok(())
    }

    #[test]
    fn test_should_create_tmux_session_command() {
        let command = TmuxSessionCommand::new(
            OsString::from("k9s"),
            vec![OsString::from("-A"), OsString::from("--readonly")],
        );

        assert_eq!(command.executable(), &OsString::from("k9s"));
        assert_eq!(
            command.args(),
            [OsString::from("-A"), OsString::from("--readonly")]
        );
    }

    #[test]
    fn test_should_parse_tmux_session_info() -> anyhow::Result<()> {
        let info = parse_session_info("presentation\t@1\t%2\t120\t40\n")?;

        assert_eq!(info.session().as_str(), "presentation");
        assert_eq!(info.window().as_str(), "@1");
        assert_eq!(info.pane().as_str(), "%2");
        assert_eq!(info.size(), TerminalSize::new(120, 40)?);
        Ok(())
    }

    async fn window_size_option(tmux: &Path, window: &BackendWindowId) -> anyhow::Result<String> {
        let output = TokioCommand::new(tmux)
            .env_remove("TMUX")
            .args([
                "show-window-options",
                "-v",
                "-t",
                window.as_str(),
                "window-size",
            ])
            .output()
            .await?;
        if !output.status.success() {
            anyhow::bail!("tmux show-window-options exited with {}", output.status);
        }
        let value = String::from_utf8(output.stdout)?;
        Ok(value.trim().to_owned())
    }

    async fn session_option(
        tmux: &Path,
        session: &SessionName,
        option: &str,
    ) -> anyhow::Result<String> {
        let output = TokioCommand::new(tmux)
            .env_remove("TMUX")
            .args(["show-options", "-v", "-t", session.as_str(), option])
            .output()
            .await?;
        if !output.status.success() {
            anyhow::bail!("tmux show-options exited with {}", output.status);
        }
        let value = String::from_utf8(output.stdout)?;
        Ok(value.trim().to_owned())
    }
}
