//! tmux backend adapter.
//!
//! This adapter is the first backend-owned session implementation. It uses tmux
//! commands to create/find a session, resolve the active pane, write input,
//! resize the pane, read the visible screen, and close the session.

use std::path::{Path, PathBuf};

use bytes::Bytes;
use tokio::process::Command;

use crate::{
    backend::{
        BackendAdapter, BackendError, BackendKind, BackendPaneId, BackendScreenSnapshot,
        BackendSessionRef, BackendWindowId,
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

    async fn ensure_session(&self, session: &SessionName) -> Result<(), BackendError> {
        if self.session_exists(session).await? {
            return Ok(());
        }
        self.create_session(session).await
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

    async fn create_session(&self, session: &SessionName) -> Result<(), BackendError> {
        let output = self
            .command()
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
            .output()
            .await
            .map_err(BackendError::Io)?;
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
        self.run(["set-option", "-t", session.as_str(), "mouse", "on"])
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
    ) -> Result<BackendSessionRef, BackendError> {
        self.ensure_session(session).await?;
        self.prepare_session(session).await?;
        let reference = self.resolve_reference(session).await?;
        self.resize(&reference, size).await?;
        Ok(reference)
    }

    async fn write_input(
        &mut self,
        target: &BackendSessionRef,
        bytes: Bytes,
    ) -> Result<(), BackendError> {
        let input =
            String::from_utf8(bytes.to_vec()).map_err(|_error| BackendError::UnsupportedInput)?;
        self.run([
            "send-keys",
            "-t",
            target.pane().as_str(),
            "-l",
            "--",
            &input,
        ])
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
        .await
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
                "#{pane_width} #{pane_height} #{cursor_x} #{cursor_y}",
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
        let output = self
            .command()
            .args(["capture-pane", "-p", "-t", target.pane().as_str()])
            .output()
            .await
            .map_err(BackendError::Io)?;
        let text = Self::success_stdout(output, "tmux capture-pane failed")?;
        let lines = text.lines().map(ToOwned::to_owned).collect();
        Ok(BackendScreenSnapshot::new(
            size, cursor_col, cursor_row, lines,
        ))
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

fn output_message(
    output: std::process::Output,
    fallback: &'static str,
) -> Result<SafeMessage, BackendError> {
    let stderr = String::from_utf8(output.stderr).map_err(BackendError::Utf8)?;
    let trimmed = stderr.trim();
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
        let reference = backend
            .create_or_find_session(&session, TerminalSize::new(80, 24)?)
            .await?;

        assert_eq!(reference.kind(), BackendKind::Tmux);
        assert_eq!(reference.session(), &session);
        assert!(!reference.window().as_str().is_empty());
        assert!(!reference.pane().as_str().is_empty());

        backend
            .write_input(
                &reference,
                Bytes::from_static(b"printf termstage-backend-ok\\n\n"),
            )
            .await?;
        sleep(Duration::from_millis(100)).await;
        let snapshot = backend.read_screen(&reference).await?;
        assert!(
            snapshot
                .lines()
                .iter()
                .any(|line| line.contains("termstage-backend-ok"))
        );

        backend
            .resize(&reference, TerminalSize::new(100, 30)?)
            .await?;
        let resized = backend.read_screen(&reference).await?;
        assert_eq!(resized.size(), TerminalSize::new(100, 30)?);

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
}
