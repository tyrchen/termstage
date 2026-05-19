//! PTY session runtime.
//!
//! The runtime owns a terminal session in a dedicated actor thread. HTTP and
//! WebSocket layers interact with it through bounded channels only.

use std::{
    collections::{HashMap, VecDeque},
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::Duration,
};

use bytes::Bytes;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use thiserror::Error;
use tokio::sync::mpsc::{self as tokio_mpsc, error::TrySendError};
use tracing::warn;

use crate::protocol::{
    ErrorCode, ProtocolError, SafeMessage, ServerControlMessage, SessionName, TerminalSize,
    WarningCode,
};

const COMMAND_MAILBOX_CAPACITY: usize = 128;
const CLIENT_OUTPUT_CAPACITY: usize = 256;
const PTY_READ_CHUNK_SIZE: usize = 8192;
const REPLAY_BUFFER_BYTES: usize = 1024 * 1024;
const ACTOR_IDLE_WAIT: Duration = Duration::from_millis(10);

/// Runtime failure.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// The configured shell command is invalid.
    #[error("invalid shell command")]
    InvalidShellCommand,
    /// The runtime could not resolve a default shell.
    #[error("failed to resolve default shell")]
    DefaultShellUnavailable,
    /// The tmux executable could not be found.
    #[error("tmux executable was not found")]
    TmuxUnavailable,
    /// Opening the PTY failed.
    #[error("failed to open pty")]
    OpenPty(#[source] anyhow::Error),
    /// Spawning the configured process failed.
    #[error("failed to spawn terminal process")]
    Spawn(#[source] anyhow::Error),
    /// Cloning the PTY reader failed.
    #[error("failed to clone pty reader")]
    CloneReader(#[source] anyhow::Error),
    /// Taking the PTY writer failed.
    #[error("failed to take pty writer")]
    TakeWriter(#[source] anyhow::Error),
    /// Spawning a runtime thread failed.
    #[error("failed to spawn runtime thread")]
    ThreadSpawn(#[source] std::io::Error),
    /// Sending a command to the actor failed because it has stopped.
    #[error("runtime actor has stopped")]
    ActorStopped,
    /// Joining the actor thread failed.
    #[error("runtime actor thread panicked")]
    ActorPanicked,
}

/// Browser/client identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientId(u64);

impl ClientId {
    /// Creates a client id.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw id.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Shell process command represented in argv form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    executable: PathBuf,
    args: Vec<OsString>,
}

impl ShellCommand {
    /// Creates a shell command from an executable path and argument vector.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InvalidShellCommand`] when `executable` is empty.
    pub fn new(
        executable: impl Into<PathBuf>,
        args: impl IntoIterator<Item = OsString>,
    ) -> Result<Self, RuntimeError> {
        let executable = executable.into();
        if executable.as_os_str().is_empty() {
            return Err(RuntimeError::InvalidShellCommand);
        }
        Ok(Self {
            executable,
            args: args.into_iter().collect(),
        })
    }

    /// Resolves the default Unix shell once at startup.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::DefaultShellUnavailable`] when no shell path can
    /// be resolved.
    pub fn default_unix() -> Result<Self, RuntimeError> {
        let shell = std::env::var_os("SHELL")
            .filter(|value| !value.is_empty())
            .map_or_else(|| PathBuf::from("/bin/sh"), PathBuf::from);
        if shell.as_os_str().is_empty() {
            Err(RuntimeError::DefaultShellUnavailable)
        } else {
            Self::new(shell, [])
        }
    }

    /// Returns the executable path.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Returns the argv tail.
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    fn command_builder(&self) -> CommandBuilder {
        let mut command = CommandBuilder::new(&self.executable);
        for arg in &self.args {
            command.arg(arg);
        }
        command
    }
}

/// Runtime session mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMode {
    /// Spawn a new local shell process.
    NewShell {
        /// Shell command in argv form.
        shell: ShellCommand,
    },
    /// Attach to or create a tmux session.
    Tmux {
        /// Validated tmux session name.
        session: SessionName,
    },
}

/// Runtime reconnect and shutdown behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectPolicy {
    /// Keep the PTY process alive across client detach where the child supports it.
    KeepAlive,
    /// Terminate the child when the runtime actor shuts down.
    TerminateOnShutdown,
}

/// Runtime configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Shell or tmux mode.
    pub mode: SessionMode,
    /// Initial PTY size.
    pub initial_size: TerminalSize,
    /// Reconnect behavior.
    pub reconnect_policy: ReconnectPolicy,
}

/// Reason for runtime shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownReason {
    /// Server supervisor requested shutdown.
    Supervisor,
    /// The browser/client disconnected.
    ClientDisconnect,
    /// The child process exited or the PTY reached EOF.
    ChildExit,
    /// Runtime error.
    RuntimeError(String),
}

/// Bounded client output sender.
pub type ClientOutputTx = tokio_mpsc::Sender<ClientOutput>;

/// Bounded client output receiver.
pub type ClientOutputRx = tokio_mpsc::Receiver<ClientOutput>;

/// Output emitted from the runtime to a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientOutput {
    /// Raw PTY bytes.
    Bytes(Bytes),
    /// Server control message.
    Control(ServerControlMessage),
    /// Runtime closed the client stream.
    Closed(ShutdownReason),
}

/// Runtime actor command.
#[derive(Debug)]
pub enum RuntimeCommand {
    /// Attach a write-capable browser client.
    AttachClient {
        /// Client id.
        client_id: ClientId,
        /// Bounded output mailbox.
        output: ClientOutputTx,
    },
    /// Detach a browser client.
    DetachClient {
        /// Client id.
        client_id: ClientId,
    },
    /// Write bytes to the PTY from the controlling client.
    Input {
        /// Client id.
        client_id: ClientId,
        /// Terminal bytes.
        bytes: Bytes,
    },
    /// Resize the PTY.
    Resize {
        /// New PTY size.
        size: TerminalSize,
    },
    /// Shut the runtime down.
    Shutdown {
        /// Shutdown reason.
        reason: ShutdownReason,
    },
}

/// Running runtime session handle.
#[derive(Debug)]
pub struct RuntimeSession {
    commands: tokio_mpsc::Sender<RuntimeCommand>,
    shutdown: mpsc::SyncSender<ShutdownReason>,
    actor: Option<JoinHandle<ActorOutcome>>,
}

impl RuntimeSession {
    /// Starts a runtime session actor.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when PTY setup or process spawning fails.
    pub fn start(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(to_pty_size(config.initial_size))
            .map_err(RuntimeError::OpenPty)?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(RuntimeError::CloneReader)?;
        let writer = pair
            .master
            .take_writer()
            .map_err(RuntimeError::TakeWriter)?;
        let child = spawn_child(&*pair.slave, &config.mode)?;
        let (command_tx, command_rx) = tokio_mpsc::channel(COMMAND_MAILBOX_CAPACITY);
        let (pty_tx, pty_rx) = mpsc::sync_channel(COMMAND_MAILBOX_CAPACITY);
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let reader = spawn_reader(reader, pty_tx)?;
        let actor = thread::Builder::new()
            .name("presenterm-pty-actor".to_owned())
            .spawn(move || {
                let actor = SessionActor {
                    config,
                    master: pair.master,
                    writer,
                    child,
                    command_rx,
                    shutdown_rx,
                    pty_rx,
                    reader,
                    clients: HashMap::new(),
                    controller: None,
                    replay: VecDeque::new(),
                    replay_bytes: 0,
                };
                actor.run()
            })
            .map_err(RuntimeError::ThreadSpawn)?;

        Ok(Self {
            commands: command_tx,
            shutdown: shutdown_tx,
            actor: Some(actor),
        })
    }

    /// Returns a clone of the command sender.
    #[must_use]
    pub fn command_sender(&self) -> tokio_mpsc::Sender<RuntimeCommand> {
        self.commands.clone()
    }

    /// Creates a bounded client output mailbox.
    #[must_use]
    pub fn client_mailbox() -> (ClientOutputTx, ClientOutputRx) {
        tokio_mpsc::channel(CLIENT_OUTPUT_CAPACITY)
    }

    /// Sends a command to the actor.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::ActorStopped`] if the actor has stopped.
    pub async fn send(&self, command: RuntimeCommand) -> Result<(), RuntimeError> {
        self.commands
            .send(command)
            .await
            .map_err(|_error| RuntimeError::ActorStopped)
    }

    /// Requests shutdown and joins the actor thread.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::ActorPanicked`] if the actor thread panicked.
    pub async fn shutdown(mut self, reason: ShutdownReason) -> Result<(), RuntimeError> {
        let _result = self.send(RuntimeCommand::Shutdown { reason }).await;
        self.join()
    }

    /// Joins the actor thread.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::ActorPanicked`] if the actor thread panicked.
    pub fn join(&mut self) -> Result<(), RuntimeError> {
        if let Some(actor) = self.actor.take() {
            actor
                .join()
                .map_err(|_error| RuntimeError::ActorPanicked)?
                .into_result()
        } else {
            Ok(())
        }
    }
}

impl Drop for RuntimeSession {
    fn drop(&mut self) {
        if let Some(actor) = self.actor.take() {
            let _result = self.shutdown.try_send(ShutdownReason::Supervisor);
            let _result = actor.join();
        }
    }
}

#[derive(Debug)]
enum PtyEvent {
    Output(Bytes),
    ReaderError(String),
    Eof,
}

#[derive(Debug)]
struct ActorOutcome(Result<(), RuntimeError>);

impl ActorOutcome {
    fn into_result(self) -> Result<(), RuntimeError> {
        self.0
    }
}

struct SessionActor {
    config: RuntimeConfig,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    command_rx: tokio_mpsc::Receiver<RuntimeCommand>,
    shutdown_rx: mpsc::Receiver<ShutdownReason>,
    pty_rx: mpsc::Receiver<PtyEvent>,
    reader: JoinHandle<()>,
    clients: HashMap<ClientId, ClientOutputTx>,
    controller: Option<ClientId>,
    replay: VecDeque<Bytes>,
    replay_bytes: usize,
}

impl SessionActor {
    fn run(mut self) -> ActorOutcome {
        let reason = self.run_loop();
        self.close_clients(&reason);
        drop(self.writer);
        let _result = self.child.kill();
        let _result = self.reader.join();
        ActorOutcome(Ok(()))
    }

    fn run_loop(&mut self) -> ShutdownReason {
        loop {
            match self.shutdown_rx.try_recv() {
                Ok(reason) => return reason,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => {}
            }

            while let Ok(event) = self.pty_rx.try_recv() {
                if let Some(reason) = self.handle_pty_event(event) {
                    return reason;
                }
            }

            match self.command_rx.try_recv() {
                Ok(command) => {
                    if let Some(reason) = self.handle_command(command) {
                        return reason;
                    }
                    continue;
                }
                Err(tokio_mpsc::error::TryRecvError::Empty) => {}
                Err(tokio_mpsc::error::TryRecvError::Disconnected) => {
                    return ShutdownReason::Supervisor;
                }
            }

            match self.pty_rx.recv_timeout(ACTOR_IDLE_WAIT) {
                Ok(event) => {
                    if let Some(reason) = self.handle_pty_event(event) {
                        return reason;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return ShutdownReason::ChildExit,
            }
        }
    }

    fn handle_command(&mut self, command: RuntimeCommand) -> Option<ShutdownReason> {
        match command {
            RuntimeCommand::AttachClient { client_id, output } => {
                self.attach_client(client_id, output);
                None
            }
            RuntimeCommand::DetachClient { client_id } => {
                self.detach_client(client_id);
                None
            }
            RuntimeCommand::Input { client_id, bytes } => {
                if self.controller == Some(client_id) {
                    match self
                        .writer
                        .write_all(&bytes)
                        .and_then(|()| self.writer.flush())
                    {
                        Ok(()) => None,
                        Err(error) => Some(ShutdownReason::RuntimeError(error.to_string())),
                    }
                } else {
                    None
                }
            }
            RuntimeCommand::Resize { size } => self
                .master
                .resize(to_pty_size(size))
                .err()
                .map(|error| ShutdownReason::RuntimeError(error.to_string())),
            RuntimeCommand::Shutdown { reason } => Some(reason),
        }
    }

    fn handle_pty_event(&mut self, event: PtyEvent) -> Option<ShutdownReason> {
        match event {
            PtyEvent::Output(bytes) => {
                self.record_replay(bytes.clone());
                self.broadcast_bytes(&bytes);
                None
            }
            PtyEvent::ReaderError(error) => Some(ShutdownReason::RuntimeError(error)),
            PtyEvent::Eof => Some(ShutdownReason::ChildExit),
        }
    }

    fn attach_client(&mut self, client_id: ClientId, output: ClientOutputTx) {
        if self
            .controller
            .is_some_and(|controller| controller != client_id)
        {
            if let Some(message) = safe_message("another controller is already attached") {
                let warning = ClientOutput::Control(ServerControlMessage::Warning {
                    code: WarningCode::ClientBackpressure,
                    message,
                });
                let _result = output.try_send(warning);
            }
            let _result = output.try_send(ClientOutput::Closed(ShutdownReason::ClientDisconnect));
            return;
        }

        self.controller = Some(client_id);
        let ready = match display_session_name(&self.config.mode) {
            Ok(session) => ClientOutput::Control(ServerControlMessage::Ready { session }),
            Err(error) => ClientOutput::Control(ServerControlMessage::Error {
                code: ErrorCode::Runtime,
                message: safe_fallback_message(error),
            }),
        };
        if output.try_send(ready).is_err() {
            self.controller = None;
            return;
        }
        for bytes in &self.replay {
            if output.try_send(ClientOutput::Bytes(bytes.clone())).is_err() {
                self.controller = None;
                return;
            }
        }
        self.clients.insert(client_id, output);
    }

    fn detach_client(&mut self, client_id: ClientId) {
        self.clients.remove(&client_id);
        if self.controller == Some(client_id) {
            self.controller = None;
        }
    }

    fn broadcast_bytes(&mut self, bytes: &Bytes) {
        let mut closed = Vec::new();
        for (client_id, output) in &self.clients {
            match output.try_send(ClientOutput::Bytes(bytes.clone())) {
                Ok(()) => {}
                Err(TrySendError::Full(_message) | TrySendError::Closed(_message)) => {
                    closed.push(*client_id);
                }
            }
        }
        for client_id in closed {
            self.close_backpressured_client(client_id);
        }
    }

    fn close_backpressured_client(&mut self, client_id: ClientId) {
        if let Some(output) = self.clients.remove(&client_id) {
            warn!(
                client_id = client_id.get(),
                "closing slow browser terminal client after output mailbox backpressure"
            );
            if let Some(message) = safe_message("browser client could not keep up") {
                let _result =
                    output.try_send(ClientOutput::Control(ServerControlMessage::Warning {
                        code: WarningCode::ClientBackpressure,
                        message,
                    }));
            }
            let _result = output.try_send(ClientOutput::Closed(ShutdownReason::ClientDisconnect));
        }
        if self.controller == Some(client_id) {
            self.controller = None;
        }
    }

    fn close_clients(&mut self, reason: &ShutdownReason) {
        for output in self.clients.values() {
            let _result = output.try_send(ClientOutput::Closed(reason.clone()));
        }
        self.clients.clear();
        self.controller = None;
    }

    fn record_replay(&mut self, bytes: Bytes) {
        self.replay_bytes = self.replay_bytes.saturating_add(bytes.len());
        self.replay.push_back(bytes);
        while self.replay_bytes > REPLAY_BUFFER_BYTES {
            if let Some(removed) = self.replay.pop_front() {
                self.replay_bytes = self.replay_bytes.saturating_sub(removed.len());
            } else {
                self.replay_bytes = 0;
                break;
            }
        }
    }
}

fn spawn_child(
    slave: &(dyn portable_pty::SlavePty + Send),
    mode: &SessionMode,
) -> Result<Box<dyn Child + Send + Sync>, RuntimeError> {
    let mut command = match mode {
        SessionMode::NewShell { shell } => shell.command_builder(),
        SessionMode::Tmux { session } => {
            let tmux = which::which("tmux").map_err(|_error| RuntimeError::TmuxUnavailable)?;
            let mut command = CommandBuilder::new(tmux);
            command.env_remove("TMUX");
            command.arg("new-session");
            command.arg("-A");
            command.arg("-s");
            command.arg(session.as_str());
            command
        }
    };
    command.env("TERM", "xterm-256color");
    slave.spawn_command(command).map_err(RuntimeError::Spawn)
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    pty_tx: mpsc::SyncSender<PtyEvent>,
) -> Result<JoinHandle<()>, RuntimeError> {
    thread::Builder::new()
        .name("presenterm-pty-reader".to_owned())
        .spawn(move || {
            let mut buffer = [0_u8; PTY_READ_CHUNK_SIZE];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _result = pty_tx.send(PtyEvent::Eof);
                        break;
                    }
                    Ok(read) => {
                        let Some(chunk) = buffer.get(..read) else {
                            let _result =
                                pty_tx.send(PtyEvent::ReaderError("invalid pty read size".into()));
                            break;
                        };
                        if pty_tx
                            .send(PtyEvent::Output(Bytes::copy_from_slice(chunk)))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _result = pty_tx.send(PtyEvent::ReaderError(error.to_string()));
                        break;
                    }
                }
            }
        })
        .map_err(RuntimeError::ThreadSpawn)
}

fn to_pty_size(size: TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows.get(),
        cols: size.cols.get(),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn display_session_name(mode: &SessionMode) -> Result<SessionName, ProtocolError> {
    match mode {
        SessionMode::NewShell { .. } => SessionName::new("shell"),
        SessionMode::Tmux { session } => Ok(session.clone()),
    }
}

fn safe_message(message: &str) -> Option<SafeMessage> {
    SafeMessage::new(message).ok()
}

fn safe_fallback_message(_error: ProtocolError) -> SafeMessage {
    SafeMessage::from_static("runtime warning")
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use anyhow::Context;
    use tokio::time::{sleep, timeout};

    use super::*;

    fn test_size() -> anyhow::Result<TerminalSize> {
        TerminalSize::new(80, 24).map_err(Into::into)
    }

    fn zsh_command() -> anyhow::Result<ShellCommand> {
        ShellCommand::new("/bin/zsh", [OsString::from("-f")]).map_err(Into::into)
    }

    async fn recv_until_contains(
        output: &mut ClientOutputRx,
        needle: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut aggregate = Vec::new();
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = timeout(remaining, output.recv())
                .await
                .context("test runtime output timed out")?
                .context("client output channel closed")?;
            if let ClientOutput::Bytes(bytes) = message {
                aggregate.extend_from_slice(&bytes);
                if aggregate
                    .windows(needle.len())
                    .any(|window| window == needle)
                {
                    return Ok(aggregate);
                }
            }
        }
        anyhow::bail!("runtime output did not contain requested marker");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_start_shell_attach_input_resize_detach_and_shutdown() -> anyhow::Result<()>
    {
        let config = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: zsh_command()?,
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
        };
        let session = RuntimeSession::start(config)?;
        let (output_tx, mut output_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(1),
                output: output_tx,
            })
            .await?;
        let ready = output_rx.recv().await.context("ready output")?;
        assert!(matches!(
            ready,
            ClientOutput::Control(ServerControlMessage::Ready { .. })
        ));

        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(1),
                bytes: Bytes::from_static(b"printf phase2-shell-ok\\n\n"),
            })
            .await?;
        let output = recv_until_contains(&mut output_rx, b"phase2-shell-ok").await?;
        assert!(
            output
                .windows(15)
                .any(|window| window == b"phase2-shell-ok")
        );

        session
            .send(RuntimeCommand::Resize {
                size: TerminalSize::new(100, 30)?,
            })
            .await?;
        session
            .send(RuntimeCommand::DetachClient {
                client_id: ClientId::new(1),
            })
            .await?;
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_report_child_exit() -> anyhow::Result<()> {
        let shell = ShellCommand::new(
            "/bin/sh",
            [
                OsString::from("-c"),
                OsString::from("printf done; sleep 0.2"),
            ],
        )?;
        let config = RuntimeConfig {
            mode: SessionMode::NewShell { shell },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
        };
        let session = RuntimeSession::start(config)?;
        let (output_tx, mut output_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(1),
                output: output_tx,
            })
            .await?;
        let _ready = output_rx.recv().await.context("ready output")?;
        let output = recv_until_contains(&mut output_rx, b"done").await?;
        assert!(output.windows(4).any(|window| window == b"done"));
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_start_tmux_mode() -> anyhow::Result<()> {
        let session_name = SessionName::new(format!("presenterm-test-{}", std::process::id()))?;
        let config = RuntimeConfig {
            mode: SessionMode::Tmux {
                session: session_name.clone(),
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
        };
        let session = RuntimeSession::start(config)?;
        let (output_tx, mut output_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(1),
                output: output_tx,
            })
            .await?;
        let ready = output_rx.recv().await.context("ready output")?;
        assert_eq!(
            ready,
            ClientOutput::Control(ServerControlMessage::Ready {
                session: session_name
            })
        );
        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(1),
                bytes: Bytes::from_static(b"printf phase2-tmux-ok\\n\n"),
            })
            .await?;
        let output = recv_until_contains(&mut output_rx, b"phase2-tmux-ok").await?;
        assert!(output.windows(14).any(|window| window == b"phase2-tmux-ok"));
        session.shutdown(ShutdownReason::Supervisor).await?;
        sleep(Duration::from_millis(50)).await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_allow_controller_reattach_after_detach() -> anyhow::Result<()> {
        let config = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: zsh_command()?,
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
        };
        let session = RuntimeSession::start(config)?;
        let (first_tx, mut first_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(1),
                output: first_tx,
            })
            .await?;
        let first_ready = first_rx.recv().await.context("first ready output")?;
        assert!(matches!(
            first_ready,
            ClientOutput::Control(ServerControlMessage::Ready { .. })
        ));
        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(1),
                bytes: Bytes::from_static(b"printf phase5-before-detach\\n\n"),
            })
            .await?;
        let first_output = recv_until_contains(&mut first_rx, b"phase5-before-detach").await?;
        assert!(
            first_output
                .windows(b"phase5-before-detach".len())
                .any(|window| window == b"phase5-before-detach")
        );
        session
            .send(RuntimeCommand::DetachClient {
                client_id: ClientId::new(1),
            })
            .await?;

        let (second_tx, mut second_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(2),
                output: second_tx,
            })
            .await?;
        let second_ready = second_rx.recv().await.context("second ready output")?;
        assert!(matches!(
            second_ready,
            ClientOutput::Control(ServerControlMessage::Ready { .. })
        ));
        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(2),
                bytes: Bytes::from_static(b"printf phase5-after-reattach\\n\n"),
            })
            .await?;
        let second_output = recv_until_contains(&mut second_rx, b"phase5-after-reattach").await?;
        assert!(
            second_output
                .windows(b"phase5-after-reattach".len())
                .any(|window| window == b"phase5-after-reattach")
        );
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_replay_recent_output_after_reattach() -> anyhow::Result<()> {
        let config = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: zsh_command()?,
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
        };
        let session = RuntimeSession::start(config)?;
        let (first_tx, mut first_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(1),
                output: first_tx,
            })
            .await?;
        let _ready = first_rx.recv().await.context("first ready output")?;
        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(1),
                bytes: Bytes::from_static(b"printf phase5-replay-state\\n\n"),
            })
            .await?;
        let first_output = recv_until_contains(&mut first_rx, b"phase5-replay-state").await?;
        assert!(
            first_output
                .windows(b"phase5-replay-state".len())
                .any(|window| window == b"phase5-replay-state")
        );
        session
            .send(RuntimeCommand::DetachClient {
                client_id: ClientId::new(1),
            })
            .await?;

        let (second_tx, mut second_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(2),
                output: second_tx,
            })
            .await?;
        let _ready = second_rx.recv().await.context("second ready output")?;
        let replayed = recv_until_contains(&mut second_rx, b"phase5-replay-state").await?;
        assert!(
            replayed
                .windows(b"phase5-replay-state".len())
                .any(|window| window == b"phase5-replay-state")
        );
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_close_slow_client_without_stopping_session() -> anyhow::Result<()> {
        let config = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: zsh_command()?,
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
        };
        let session = RuntimeSession::start(config)?;
        let (slow_tx, mut slow_rx) = tokio_mpsc::channel(1);
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(1),
                output: slow_tx,
            })
            .await?;
        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(1),
                bytes: Bytes::from_static(b"printf 'phase5-backpressure-%s\\n' {1..300}\n"),
            })
            .await?;
        sleep(Duration::from_millis(500)).await;
        let ready = slow_rx.recv().await.context("slow client ready output")?;
        assert!(matches!(
            ready,
            ClientOutput::Control(ServerControlMessage::Ready { .. })
        ));
        assert!(
            timeout(Duration::from_secs(2), slow_rx.recv())
                .await?
                .is_none()
        );

        let (recovered_tx, mut recovered_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(2),
                output: recovered_tx,
            })
            .await?;
        let recovered_ready = recovered_rx
            .recv()
            .await
            .context("recovered ready output")?;
        assert!(matches!(
            recovered_ready,
            ClientOutput::Control(ServerControlMessage::Ready { .. })
        ));
        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(2),
                bytes: Bytes::from_static(b"printf phase5-session-alive\\n\n"),
            })
            .await?;
        let recovered_output =
            recv_until_contains(&mut recovered_rx, b"phase5-session-alive").await?;
        assert!(
            recovered_output
                .windows(b"phase5-session-alive".len())
                .any(|window| window == b"phase5-session-alive")
        );
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }
}
