//! rmux backend adapter.
//!
//! This adapter talks to the daemon-backed `rmux-sdk` API directly. rmux owns
//! the real session, pane, PTY, screen parser, output retention, and native
//! local attach path; `termstage` keeps the browser/API registry and Level 1
//! operation lock above this adapter.

use std::{collections::HashMap, env, ffi::OsString, time::Duration};

use bytes::Bytes;
use rmux_sdk::{
    EnsureSession, EnsureSessionPolicy, Pane, PaneId, Rmux, RmuxError, Session as RmuxSession,
    SessionName as RmuxSessionName, TerminalSizeSpec,
};

use crate::{
    backend::{
        BackendAdapter, BackendError, BackendKind, BackendPaneId, BackendScreenSnapshot,
        BackendScrollDirection, BackendSessionRef, BackendSessionResolution, BackendWindowId,
    },
    protocol::{SafeMessage, SessionName, TerminalSize},
    settings::rmux_backend as rmux_settings,
};

/// rmux backend adapter configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RmuxBackendConfig {
    /// Default timeout applied to rmux SDK daemon operations.
    pub default_operation_timeout: Duration,
    /// Whether [`BackendAdapter::close_session`] kills sessions that this
    /// adapter only reused instead of creating.
    pub close_reused_sessions: bool,
}

impl Default for RmuxBackendConfig {
    fn default() -> Self {
        Self {
            default_operation_timeout: rmux_settings::DEFAULT_OPERATION_TIMEOUT,
            close_reused_sessions: false,
        }
    }
}

/// Command used to start a new rmux session pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RmuxSessionCommand {
    argv: Vec<String>,
}

impl RmuxSessionCommand {
    /// Creates an rmux session command after validating all argv items as
    /// UTF-8 strings.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::UnsupportedInput`] when any argv item is not
    /// valid UTF-8, or [`BackendError::Operation`] when the executable is empty.
    pub fn try_new(
        executable: OsString,
        command_args: Vec<OsString>,
    ) -> Result<Self, BackendError> {
        let executable = os_string_to_string(executable)?;
        if executable.is_empty() {
            return Err(BackendError::Operation(SafeMessage::from_static(
                "rmux command executable must not be empty",
            )));
        }
        let mut argv = Vec::with_capacity(command_args.len().saturating_add(1));
        argv.push(executable);
        for arg in command_args {
            argv.push(os_string_to_string(arg)?);
        }
        Ok(Self { argv })
    }

    /// Returns the argv vector passed to rmux.
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }
}

/// rmux session details used by supervisor CLI commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RmuxSessionInfo {
    session: SessionName,
    window: BackendWindowId,
    pane: BackendPaneId,
    size: TerminalSize,
}

impl RmuxSessionInfo {
    /// Creates rmux session details.
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

/// rmux backend adapter.
#[derive(Debug)]
pub struct RmuxBackend {
    rmux: Rmux,
    bindings: HashMap<SessionName, RmuxSessionBinding>,
    config: RmuxBackendConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RmuxSessionBinding {
    backend_ref: BackendSessionRef,
    created_by_adapter: bool,
}

impl RmuxBackend {
    /// Connects to an existing rmux daemon.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the daemon cannot be reached.
    pub async fn connect() -> Result<Self, BackendError> {
        Self::connect_with_config(RmuxBackendConfig::default()).await
    }

    /// Connects to an existing rmux daemon with explicit adapter settings.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the daemon cannot be reached.
    pub async fn connect_with_config(config: RmuxBackendConfig) -> Result<Self, BackendError> {
        let rmux = Rmux::builder()
            .default_timeout(config.default_operation_timeout)
            .connect()
            .await
            .map_err(map_rmux_error)?;
        Ok(Self::new(rmux, config))
    }

    /// Connects to rmux, starting the hidden daemon when needed.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when rmux cannot connect or start.
    pub async fn connect_or_start() -> Result<Self, BackendError> {
        Self::connect_or_start_with_config(RmuxBackendConfig::default()).await
    }

    /// Connects to rmux with explicit adapter settings, starting the hidden
    /// daemon when needed.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when rmux cannot connect or start.
    pub async fn connect_or_start_with_config(
        config: RmuxBackendConfig,
    ) -> Result<Self, BackendError> {
        let rmux = Rmux::builder()
            .default_timeout(config.default_operation_timeout)
            .connect_or_start()
            .await
            .map_err(map_rmux_error)?;
        Ok(Self::new(rmux, config))
    }

    /// Creates an adapter from an existing rmux facade.
    #[must_use]
    pub fn new(rmux: Rmux, config: RmuxBackendConfig) -> Self {
        Self {
            rmux,
            bindings: HashMap::new(),
            config,
        }
    }

    /// Lists rmux sessions whose names are valid termstage session ids.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when rmux cannot list sessions.
    pub async fn list_sessions(&self) -> Result<Vec<SessionName>, BackendError> {
        let sessions = self.rmux.list_sessions().await.map_err(map_rmux_error)?;
        Ok(sessions
            .iter()
            .filter_map(|session| SessionName::new(session.as_str()).ok())
            .collect())
    }

    /// Reads details for an rmux session.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the session cannot be resolved.
    pub async fn inspect_session(
        &self,
        session: &SessionName,
    ) -> Result<RmuxSessionInfo, BackendError> {
        let rmux_session = self.rmux_session(session).await?;
        self.session_info(&rmux_session).await
    }

    /// Returns whether an rmux session exists.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when rmux cannot check the session.
    pub async fn session_exists_by_name(
        &self,
        session: &SessionName,
    ) -> Result<bool, BackendError> {
        let rmux_name = to_rmux_session_name(session)?;
        self.rmux
            .has_session(rmux_name)
            .await
            .map_err(map_rmux_error)
    }

    /// Creates a new rmux session and starts `command` as the first pane
    /// command. When `command` is `None`, termstage starts the current
    /// `$SHELL`, falling back to `/bin/sh`.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the session already exists or rmux cannot
    /// create or inspect the session.
    pub async fn create_new_session(
        &mut self,
        session: &SessionName,
        size: TerminalSize,
        command: Option<&RmuxSessionCommand>,
    ) -> Result<RmuxSessionInfo, BackendError> {
        if self.session_exists_by_name(session).await? {
            return Err(BackendError::Operation(SafeMessage::from_static(
                "rmux session already exists",
            )));
        }
        let rmux_session = self
            .ensure_session(session, size, EnsureSessionPolicy::CreateOnly, command)
            .await?;
        let info = self.session_info(&rmux_session).await?;
        self.remember_binding(&info, true);
        Ok(info)
    }

    /// Resolves an existing rmux session for gateway attachment.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::SessionNotFound`] when the session does not
    /// exist, or another [`BackendError`] when rmux cannot inspect it.
    pub async fn attach_existing_session(
        &self,
        session: &SessionName,
    ) -> Result<BackendSessionRef, BackendError> {
        let rmux_session = self.rmux_session(session).await?;
        Ok(self.session_info(&rmux_session).await?.backend_ref())
    }

    /// Kills an rmux session by name.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when rmux cannot kill the session.
    pub async fn kill_session_by_name(&self, session: &SessionName) -> Result<bool, BackendError> {
        let rmux_session = self.rmux_session(session).await?;
        rmux_session.kill().await.map_err(map_rmux_error)
    }

    async fn rmux_session(&self, session: &SessionName) -> Result<RmuxSession, BackendError> {
        let rmux_name = to_rmux_session_name(session)?;
        self.rmux.session(rmux_name).await.map_err(map_rmux_error)
    }

    async fn ensure_session(
        &mut self,
        session: &SessionName,
        size: TerminalSize,
        policy: EnsureSessionPolicy,
        command: Option<&RmuxSessionCommand>,
    ) -> Result<RmuxSession, BackendError> {
        let rmux_name = to_rmux_session_name(session)?;
        let working_directory = inherited_working_directory()?;
        let shell_executable = inherited_shell_executable()?;
        let default_command;
        let command = if let Some(command) = command {
            command
        } else {
            default_command = default_shell_command_from_executable(&shell_executable)?;
            &default_command
        };
        let ensure = EnsureSession::named(rmux_name)
            .policy(policy)
            .detached(true)
            .size(to_rmux_terminal_size(size))
            .working_directory(working_directory)
            .environment(rmux_session_environment(&shell_executable))
            .argv(command.argv().iter().cloned());
        self.rmux
            .ensure_session(ensure)
            .await
            .map_err(map_rmux_error)
    }

    async fn session_info(&self, session: &RmuxSession) -> Result<RmuxSessionInfo, BackendError> {
        let termstage_session = SessionName::new(session.name().as_str().to_owned())?;
        let pane = session.pane(0, 0);
        let pane_id = pane.id().await.map_err(map_rmux_error)?;
        let pane_id = pane_id.ok_or(BackendError::SessionNotFound)?;
        let snapshot = pane.snapshot().await.map_err(map_rmux_error)?;
        let size = TerminalSize::new(snapshot.cols, snapshot.rows)?;
        let backend_ref = rmux_backend_ref(&termstage_session, pane_id)?;
        Ok(RmuxSessionInfo::new(
            termstage_session,
            backend_ref.window().clone(),
            backend_ref.pane().clone(),
            size,
        ))
    }

    async fn pane_for_target(&self, target: &BackendSessionRef) -> Result<Pane, BackendError> {
        if target.kind() != BackendKind::Rmux {
            return Err(BackendError::Operation(SafeMessage::from_static(
                "backend target is not an rmux pane",
            )));
        }
        let rmux_name = to_rmux_session_name(target.session())?;
        let pane_id = pane_id_from_backend(target.pane())?;
        self.rmux
            .pane_by_id(rmux_name, pane_id)
            .await
            .map_err(map_rmux_error)
    }

    fn remember_binding(&mut self, info: &RmuxSessionInfo, created_by_adapter: bool) {
        let backend_ref = BackendSessionRef::new(
            BackendKind::Rmux,
            info.session().clone(),
            info.window().clone(),
            info.pane().clone(),
        );
        self.bindings.insert(
            info.session().clone(),
            RmuxSessionBinding {
                backend_ref,
                created_by_adapter,
            },
        );
    }
}

impl BackendAdapter for RmuxBackend {
    async fn create_or_find_session(
        &mut self,
        session: &SessionName,
        size: TerminalSize,
    ) -> Result<BackendSessionResolution, BackendError> {
        let rmux_session = self
            .ensure_session(session, size, EnsureSessionPolicy::CreateOrReuse, None)
            .await?;
        let created = rmux_session.was_created();
        let info = self.session_info(&rmux_session).await?;
        self.remember_binding(&info, created);
        let binding = self
            .bindings
            .get(info.session())
            .ok_or(BackendError::SessionNotFound)?;
        Ok(BackendSessionResolution::new(
            binding.backend_ref.clone(),
            created,
        ))
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
        self.pane_for_target(target)
            .await?
            .send_text(text)
            .await
            .map_err(map_rmux_error)
    }

    async fn send_key(
        &mut self,
        target: &BackendSessionRef,
        key: &str,
    ) -> Result<(), BackendError> {
        self.pane_for_target(target)
            .await?
            .send_key(key)
            .await
            .map_err(map_rmux_error)
    }

    async fn run_command(
        &mut self,
        target: &BackendSessionRef,
        command: &str,
    ) -> Result<(), BackendError> {
        let pane = self.pane_for_target(target).await?;
        pane.send_text(command).await.map_err(map_rmux_error)?;
        pane.send_key("Enter").await.map_err(map_rmux_error)
    }

    async fn resize(
        &mut self,
        target: &BackendSessionRef,
        size: TerminalSize,
    ) -> Result<(), BackendError> {
        self.pane_for_target(target)
            .await?
            .resize(to_rmux_terminal_size(size))
            .await
            .map_err(map_rmux_error)
    }

    async fn read_screen(
        &mut self,
        target: &BackendSessionRef,
    ) -> Result<BackendScreenSnapshot, BackendError> {
        let snapshot = self
            .pane_for_target(target)
            .await?
            .snapshot()
            .await
            .map_err(map_rmux_error)?;
        let size = TerminalSize::new(snapshot.cols, snapshot.rows)?;
        Ok(BackendScreenSnapshot::new_with_cursor_visibility(
            size,
            snapshot.cursor.col,
            snapshot.cursor.row,
            snapshot.cursor.visible,
            snapshot.visible_lines(),
        ))
    }

    async fn has_native_client(
        &mut self,
        target: &BackendSessionRef,
    ) -> Result<bool, BackendError> {
        let info = self
            .pane_for_target(target)
            .await?
            .info()
            .await
            .map_err(map_rmux_error)?;
        Ok(info.sessions.iter().any(|session| {
            session.name.as_str() == target.session().as_str() && session.attached_clients > 0
        }))
    }

    async fn scroll(
        &mut self,
        _target: &BackendSessionRef,
        _direction: BackendScrollDirection,
        _amount: u16,
    ) -> Result<(), BackendError> {
        Err(BackendError::Operation(SafeMessage::from_static(
            "rmux backend does not expose a scroll primitive",
        )))
    }

    async fn close_session(&mut self, target: &BackendSessionRef) -> Result<(), BackendError> {
        let should_kill = self
            .bindings
            .get(target.session())
            .is_some_and(|binding| binding.created_by_adapter)
            || self.config.close_reused_sessions;
        if should_kill {
            let rmux_session = self.rmux_session(target.session()).await?;
            let _killed = rmux_session.kill().await.map_err(map_rmux_error)?;
        }
        self.bindings.remove(target.session());
        Ok(())
    }
}

impl RmuxSessionInfo {
    fn backend_ref(&self) -> BackendSessionRef {
        BackendSessionRef::new(
            BackendKind::Rmux,
            self.session.clone(),
            self.window.clone(),
            self.pane.clone(),
        )
    }
}

fn os_string_to_string(value: OsString) -> Result<String, BackendError> {
    value
        .into_string()
        .map_err(|_value| BackendError::UnsupportedInput)
}

fn inherited_working_directory() -> Result<String, BackendError> {
    let current_directory = env::current_dir().map_err(BackendError::Io)?;
    os_string_to_string(current_directory.into_os_string())
}

fn inherited_shell_executable() -> Result<String, BackendError> {
    inherited_shell_executable_from_env(env::var_os("SHELL"))
}

fn inherited_shell_executable_from_env(shell: Option<OsString>) -> Result<String, BackendError> {
    let executable = shell
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| OsString::from("/bin/sh"));
    os_string_to_string(executable)
}

fn default_shell_command_from_executable(
    executable: &str,
) -> Result<RmuxSessionCommand, BackendError> {
    RmuxSessionCommand::try_new(OsString::from(executable), Vec::new())
}

fn rmux_session_environment(shell_executable: &str) -> Vec<String> {
    vec![
        format!("SHELL={shell_executable}"),
        format!("COLORTERM={}", rmux_settings::TERMINAL_COLOR_MODE),
        "CLICOLOR=1".to_owned(),
        format!("TERM_PROGRAM={}", rmux_settings::TERMINAL_PROGRAM),
        format!("TERM_PROGRAM_VERSION={}", env!("CARGO_PKG_VERSION")),
    ]
}

fn to_rmux_session_name(session: &SessionName) -> Result<RmuxSessionName, BackendError> {
    let requested = session.as_str();
    let rmux_name = RmuxSessionName::new(requested.to_owned())
        .map_err(|error| operation_error(&error.to_string(), "invalid rmux session name"))?;
    if rmux_name.as_str() != requested {
        return Err(BackendError::Operation(SafeMessage::from_static(
            "rmux would rewrite this session name",
        )));
    }
    Ok(rmux_name)
}

fn to_rmux_terminal_size(size: TerminalSize) -> TerminalSizeSpec {
    TerminalSizeSpec::new(size.cols.get(), size.rows.get())
}

fn rmux_backend_ref(
    session: &SessionName,
    pane_id: PaneId,
) -> Result<BackendSessionRef, BackendError> {
    Ok(BackendSessionRef::new(
        BackendKind::Rmux,
        session.clone(),
        BackendWindowId::new("0")?,
        BackendPaneId::new(pane_id.to_string())?,
    ))
}

fn pane_id_from_backend(pane: &BackendPaneId) -> Result<PaneId, BackendError> {
    let value = pane.as_str();
    let digits = value
        .strip_prefix('%')
        .ok_or_else(|| BackendError::Operation(SafeMessage::from_static("invalid rmux pane id")))?;
    let raw = digits.parse::<u32>().map_err(|_error| {
        BackendError::Operation(SafeMessage::from_static("invalid rmux pane id"))
    })?;
    Ok(PaneId::new(raw))
}

fn map_rmux_error(error: RmuxError) -> BackendError {
    let message = error.to_string();
    if message.contains("session not found")
        || message.contains("no such session")
        || message.contains("pane not found")
    {
        return BackendError::SessionNotFound;
    }
    match error {
        RmuxError::Transport { source, .. } => BackendError::Io(source),
        _ => operation_error(&message, "rmux operation failed"),
    }
}

fn operation_error(message: &str, fallback: &'static str) -> BackendError {
    BackendError::Operation(safe_message(message, fallback))
}

fn safe_message(message: &str, fallback: &'static str) -> SafeMessage {
    let trimmed = message.trim();
    let candidate = if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    };
    if let Ok(message) = SafeMessage::new(candidate.to_owned()) {
        return message;
    }
    let mut truncated = String::new();
    for character in candidate.chars() {
        if truncated.len().saturating_add(character.len_utf8()) > 512 {
            break;
        }
        truncated.push(character);
    }
    match SafeMessage::new(truncated) {
        Ok(message) => message,
        Err(_error) => SafeMessage::from_static(fallback),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TerminalSize;

    #[test]
    fn test_should_convert_terminal_size_to_rmux_size() -> anyhow::Result<()> {
        let size = to_rmux_terminal_size(TerminalSize::new(120, 30)?);

        assert_eq!(size.cols, 120);
        assert_eq!(size.rows, 30);
        Ok(())
    }

    #[test]
    fn test_should_reject_rmux_session_names_that_would_be_rewritten() -> anyhow::Result<()> {
        let session = SessionName::new("demo.with.dot")?;
        let error = to_rmux_session_name(&session);

        assert!(matches!(error, Err(BackendError::Operation(_))));
        Ok(())
    }

    #[test]
    fn test_should_parse_rmux_pane_id() -> anyhow::Result<()> {
        let pane = BackendPaneId::new("%42")?;

        assert_eq!(pane_id_from_backend(&pane)?.as_u32(), 42);
        Ok(())
    }

    #[test]
    fn test_should_reject_invalid_rmux_pane_id() -> anyhow::Result<()> {
        let pane = BackendPaneId::new("42")?;

        assert!(matches!(
            pane_id_from_backend(&pane),
            Err(BackendError::Operation(_))
        ));
        Ok(())
    }

    #[test]
    fn test_should_build_utf8_rmux_session_command() -> anyhow::Result<()> {
        let command =
            RmuxSessionCommand::try_new(OsString::from("k9s"), vec![OsString::from("--readonly")])?;

        assert_eq!(command.argv(), ["k9s", "--readonly"]);
        Ok(())
    }

    #[test]
    fn test_should_choose_rmux_default_shell_from_environment() -> anyhow::Result<()> {
        let shell = inherited_shell_executable_from_env(Some(OsString::from("/bin/zsh")))?;
        let command = default_shell_command_from_executable(&shell)?;

        assert_eq!(shell, "/bin/zsh");
        assert_eq!(command.argv(), ["/bin/zsh"]);
        Ok(())
    }

    #[test]
    fn test_should_fallback_to_bin_sh_for_missing_rmux_shell() -> anyhow::Result<()> {
        let missing_shell = inherited_shell_executable_from_env(None)?;
        let empty_shell = inherited_shell_executable_from_env(Some(OsString::new()))?;

        assert_eq!(missing_shell, "/bin/sh");
        assert_eq!(empty_shell, "/bin/sh");
        Ok(())
    }

    #[test]
    fn test_should_include_shell_in_rmux_session_environment() {
        let environment = rmux_session_environment("/bin/zsh");

        assert!(environment.iter().any(|entry| entry == "SHELL=/bin/zsh"));
        assert!(environment.iter().any(|entry| entry == "CLICOLOR=1"));
    }
}
