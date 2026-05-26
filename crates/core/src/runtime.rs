//! PTY session runtime.
//!
//! The runtime owns a terminal session in a dedicated actor thread. HTTP and
//! WebSocket layers interact with it through bounded channels only.

use std::{
    collections::{HashMap, VecDeque},
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::mpsc::{self, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::Duration,
};

use bytes::Bytes;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, SlavePty, native_pty_system};
use thiserror::Error;
use tokio::sync::mpsc::{self as tokio_mpsc, error::TrySendError};
use tracing::warn;

use crate::protocol::{
    ErrorCode, LeaseOwner, ProtocolError, SafeMessage, ServerControlMessage, SessionName,
    TerminalSize, WarningCode,
};

const COMMAND_MAILBOX_CAPACITY: usize = 128;
const CLIENT_OUTPUT_CAPACITY: usize = 256;
const PTY_READ_CHUNK_SIZE: usize = 8192;
const REPLAY_BUFFER_BYTES: usize = 1024 * 1024;
const REPLAY_BUFFER_CHUNKS: usize = CLIENT_OUTPUT_CAPACITY / 2;
const ACTOR_IDLE_WAIT: Duration = Duration::from_millis(10);
const TERMINAL_TERM: &str = "xterm-256color";
const TERMINAL_COLOR_MODE: &str = "truecolor";
const TERMINAL_PROGRAM: &str = "termstage";
const TMUX_HISTORY_LIMIT: &str = "100000";
const DISABLE_COLOR_ENV: [&str; 2] = ["NO_COLOR", "ANSI_COLORS_DISABLED"];

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

/// Behavior after the terminal child exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitPolicy {
    /// Keep the browser session open and report the exited process.
    Hold,
    /// Close the browser session when the terminal child exits.
    End,
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
    /// Child process exit behavior.
    pub exit_policy: ExitPolicy,
}

/// Reason for runtime shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownReason {
    /// Server supervisor requested shutdown.
    Supervisor,
    /// The browser/client disconnected.
    ClientDisconnect,
    /// A newer browser/client took over the controller role.
    ControllerReplaced,
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
    /// Attach the local terminal frontend.
    AttachTerminal {
        /// Bounded output mailbox.
        output: ClientOutputTx,
    },
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
    /// Write bytes to the PTY from the local terminal frontend.
    TerminalInput {
        /// Terminal bytes.
        bytes: Bytes,
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
    /// Resize the PTY from a browser client.
    BrowserResize {
        /// Client id.
        client_id: ClientId,
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
        let reader = spawn_reader(reader, pty_tx.clone(), 0)?;
        let initial_size = config.initial_size;
        let actor = thread::Builder::new()
            .name("termstage-pty-actor".to_owned())
            .spawn(move || {
                let actor = SessionActor {
                    config,
                    slave: pair.slave,
                    master: pair.master,
                    writer,
                    child,
                    command_rx,
                    shutdown_rx,
                    pty_rx,
                    pty_tx,
                    reader: Some(reader),
                    reader_generation: 0,
                    terminal: None,
                    clients: HashMap::new(),
                    client_sizes: HashMap::new(),
                    browser_controller: None,
                    lease: InputLease::terminal(),
                    child_exited: false,
                    current_size: initial_size,
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

    /// Returns whether the runtime actor thread has exited.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.actor
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
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
struct PtyEvent {
    generation: u64,
    kind: PtyEventKind,
}

#[derive(Debug)]
enum PtyEventKind {
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
    slave: Box<dyn SlavePty + Send>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    command_rx: tokio_mpsc::Receiver<RuntimeCommand>,
    shutdown_rx: mpsc::Receiver<ShutdownReason>,
    pty_rx: mpsc::Receiver<PtyEvent>,
    pty_tx: mpsc::SyncSender<PtyEvent>,
    reader: Option<JoinHandle<()>>,
    reader_generation: u64,
    terminal: Option<ClientOutputTx>,
    clients: HashMap<ClientId, ClientOutputTx>,
    client_sizes: HashMap<ClientId, TerminalSize>,
    browser_controller: Option<ClientId>,
    lease: InputLease,
    child_exited: bool,
    current_size: TerminalSize,
    replay: VecDeque<Bytes>,
    replay_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InputLease {
    owner: InputLeaseOwner,
    epoch: u64,
}

impl InputLease {
    const fn terminal() -> Self {
        Self {
            owner: InputLeaseOwner::Terminal,
            epoch: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputLeaseOwner {
    Terminal,
    Browser(ClientId),
}

impl InputLeaseOwner {
    const fn protocol_owner(self) -> LeaseOwner {
        match self {
            Self::Terminal => LeaseOwner::Terminal,
            Self::Browser(_client_id) => LeaseOwner::Browser,
        }
    }
}

impl SessionActor {
    fn run(mut self) -> ActorOutcome {
        let reason = self.run_loop();
        self.close_clients(&reason);
        let Self {
            slave,
            master,
            writer,
            mut child,
            reader,
            ..
        } = self;
        let _result = child.kill();
        drop(child);
        drop(writer);
        drop(master);
        drop(slave);
        if let Some(reader) = reader {
            let _result = reader.join();
        }
        ActorOutcome(Ok(()))
    }

    fn run_loop(&mut self) -> ShutdownReason {
        loop {
            match self.shutdown_rx.try_recv() {
                Ok(reason) => return reason,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => {}
            }

            if !self.child_exited {
                while let Ok(event) = self.pty_rx.try_recv() {
                    if let Some(reason) = self.handle_pty_event(event) {
                        return reason;
                    }
                }
                if let Some(reason) = self.poll_child_exit() {
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

            if self.child_exited {
                thread::sleep(ACTOR_IDLE_WAIT);
            } else {
                match self.pty_rx.recv_timeout(ACTOR_IDLE_WAIT) {
                    Ok(event) => {
                        if let Some(reason) = self.handle_pty_event(event) {
                            return reason;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        if let Some(reason) = self.handle_child_exit() {
                            return reason;
                        }
                    }
                }
            }
        }
    }

    fn handle_child_exit(&mut self) -> Option<ShutdownReason> {
        if self.child_exited {
            return None;
        }

        match self.config.exit_policy {
            ExitPolicy::End => Some(ShutdownReason::ChildExit),
            ExitPolicy::Hold => {
                self.child_exited = true;
                self.notify_process_exited();
                None
            }
        }
    }

    fn poll_child_exit(&mut self) -> Option<ShutdownReason> {
        match self.child.try_wait() {
            Ok(Some(_status)) => self.handle_child_exit(),
            Ok(None) => None,
            Err(error) => Some(ShutdownReason::RuntimeError(error.to_string())),
        }
    }

    fn handle_reader_error(&mut self, error: String) -> Option<ShutdownReason> {
        match self.child.try_wait() {
            Ok(Some(_status)) => self.handle_child_exit(),
            Ok(None) | Err(_) => Some(ShutdownReason::RuntimeError(error)),
        }
    }

    fn restart_child(&mut self) -> Result<(), RuntimeError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(to_pty_size(self.current_size))
            .map_err(RuntimeError::OpenPty)?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(RuntimeError::CloneReader)?;
        let writer = pair
            .master
            .take_writer()
            .map_err(RuntimeError::TakeWriter)?;
        let child = spawn_child(&*pair.slave, &self.config.mode)?;
        let next_generation = self.reader_generation.saturating_add(1);
        let old_reader =
            self.reader
                .replace(spawn_reader(reader, self.pty_tx.clone(), next_generation)?);
        self.reader_generation = next_generation;
        self.slave = pair.slave;
        self.master = pair.master;
        self.writer = writer;
        self.child = child;
        if let Some(reader) = old_reader {
            let _result = reader.join();
        }
        self.child_exited = false;
        self.replay.clear();
        self.replay_bytes = 0;
        Ok(())
    }

    fn notify_process_exited(&mut self) {
        let message = ClientOutput::Control(ServerControlMessage::ProcessExited {
            message: SafeMessage::from_static("The terminal process exited."),
        });
        let mut closed = Vec::new();
        if let Some(output) = &self.terminal
            && output.try_send(message.clone()).is_err()
        {
            self.terminal = None;
        }
        for (client_id, output) in &self.clients {
            if output.try_send(message.clone()).is_err() {
                closed.push(*client_id);
            }
        }
        for client_id in closed {
            self.clients.remove(&client_id);
            if self.browser_controller == Some(client_id) {
                self.browser_controller = None;
            }
        }
    }

    fn handle_command(&mut self, command: RuntimeCommand) -> Option<ShutdownReason> {
        match command {
            RuntimeCommand::AttachTerminal { output } => self.attach_terminal(output),
            RuntimeCommand::AttachClient { client_id, output } => {
                self.attach_client(client_id, output)
            }
            RuntimeCommand::DetachClient { client_id } => {
                self.detach_client(client_id);
                None
            }
            RuntimeCommand::TerminalInput { bytes } => {
                if self.terminal.is_some() && !self.child_exited {
                    self.claim_terminal();
                    self.write_input(&bytes)
                } else {
                    None
                }
            }
            RuntimeCommand::Input { client_id, bytes } => {
                if self.browser_controller == Some(client_id) && !self.child_exited {
                    self.claim_browser(client_id);
                    self.write_input(&bytes)
                } else {
                    None
                }
            }
            RuntimeCommand::Resize { size } => self.resize(size),
            RuntimeCommand::BrowserResize { client_id, size } => {
                self.client_sizes.insert(client_id, size);
                if self.should_apply_browser_resize(client_id) {
                    self.resize(size)
                } else {
                    None
                }
            }
            RuntimeCommand::Shutdown { reason } => Some(reason),
        }
    }

    fn resize(&mut self, size: TerminalSize) -> Option<ShutdownReason> {
        self.current_size = size;
        let resize_result = if self.child_exited {
            Ok(())
        } else {
            self.master
                .resize(to_pty_size(size))
                .map_err(|error| ShutdownReason::RuntimeError(error.to_string()))
        };
        match resize_result {
            Ok(()) => {
                self.broadcast_size();
                None
            }
            Err(reason) => Some(reason),
        }
    }

    fn write_input(&mut self, bytes: &[u8]) -> Option<ShutdownReason> {
        self.writer
            .write_all(bytes)
            .and_then(|()| self.writer.flush())
            .err()
            .map(|error| ShutdownReason::RuntimeError(error.to_string()))
    }

    fn handle_pty_event(&mut self, event: PtyEvent) -> Option<ShutdownReason> {
        if event.generation != self.reader_generation {
            return None;
        }

        match event.kind {
            PtyEventKind::Output(bytes) => {
                self.record_replay(bytes.clone());
                self.broadcast_bytes(&bytes);
                None
            }
            PtyEventKind::ReaderError(error) => self.handle_reader_error(error),
            PtyEventKind::Eof => self.handle_child_exit(),
        }
    }

    fn attach_client(
        &mut self,
        client_id: ClientId,
        output: ClientOutputTx,
    ) -> Option<ShutdownReason> {
        if self.child_exited
            && let Err(error) = self.restart_child()
        {
            let reason = ShutdownReason::RuntimeError(error.to_string());
            let _result = output.try_send(ClientOutput::Closed(reason.clone()));
            return Some(reason);
        }

        match self.browser_controller {
            Some(controller) if controller != client_id => {
                if let Some(previous) = self.clients.remove(&controller) {
                    let _result =
                        previous.try_send(ClientOutput::Closed(ShutdownReason::ControllerReplaced));
                }
                self.client_sizes.remove(&controller);
            }
            Some(_) | None => {}
        }

        self.browser_controller = Some(client_id);
        let ready = match display_session_name(&self.config.mode) {
            Ok(session) => ClientOutput::Control(ServerControlMessage::Ready { session }),
            Err(error) => ClientOutput::Control(ServerControlMessage::Error {
                code: ErrorCode::Runtime,
                message: safe_fallback_message(error),
            }),
        };
        if output.try_send(ready).is_err() {
            self.browser_controller = None;
            return None;
        }
        if output.try_send(self.current_size_message()).is_err() {
            self.browser_controller = None;
            return None;
        }
        if self.should_send_lease_to_browser() && self.send_current_lease_to(&output).is_err() {
            self.browser_controller = None;
            return None;
        }
        if output
            .try_send(ClientOutput::Control(ServerControlMessage::ReplayStarted))
            .is_err()
        {
            self.browser_controller = None;
            return None;
        }
        for bytes in &self.replay {
            if output.try_send(ClientOutput::Bytes(bytes.clone())).is_err() {
                self.browser_controller = None;
                return None;
            }
        }
        if output
            .try_send(ClientOutput::Control(ServerControlMessage::ReplayFinished))
            .is_err()
        {
            self.browser_controller = None;
            return None;
        }
        self.clients.insert(client_id, output);
        None
    }

    fn attach_terminal(&mut self, output: ClientOutputTx) -> Option<ShutdownReason> {
        if self.child_exited
            && let Err(error) = self.restart_child()
        {
            let reason = ShutdownReason::RuntimeError(error.to_string());
            let _result = output.try_send(ClientOutput::Closed(reason.clone()));
            return Some(reason);
        }

        self.terminal = Some(output);
        if self.lease.owner == InputLeaseOwner::Terminal {
            self.broadcast_lease();
        } else {
            self.claim_terminal();
        }
        let replay: Vec<Bytes> = self.replay.iter().cloned().collect();
        for bytes in replay {
            let Some(output) = &self.terminal else {
                return None;
            };
            if output.try_send(ClientOutput::Bytes(bytes)).is_err() {
                self.terminal = None;
                return None;
            }
        }
        None
    }

    fn should_send_lease_to_browser(&self) -> bool {
        self.terminal.is_some() || matches!(self.lease.owner, InputLeaseOwner::Browser(_))
    }

    fn should_apply_browser_resize(&self, client_id: ClientId) -> bool {
        self.terminal.is_none() && self.browser_controller == Some(client_id)
    }

    fn detach_client(&mut self, client_id: ClientId) {
        self.clients.remove(&client_id);
        self.client_sizes.remove(&client_id);
        if self.browser_controller == Some(client_id) {
            self.browser_controller = None;
        }
    }

    fn broadcast_bytes(&mut self, bytes: &Bytes) {
        let mut closed = Vec::new();
        if let Some(output) = &self.terminal
            && output.try_send(ClientOutput::Bytes(bytes.clone())).is_err()
        {
            self.terminal = None;
        }
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
        if self.browser_controller == Some(client_id) {
            self.browser_controller = None;
        }
        self.client_sizes.remove(&client_id);
    }

    fn close_clients(&mut self, reason: &ShutdownReason) {
        if let Some(output) = self.terminal.take() {
            let _result = output.try_send(ClientOutput::Closed(reason.clone()));
        }
        for output in self.clients.values() {
            let _result = output.try_send(ClientOutput::Closed(reason.clone()));
        }
        self.clients.clear();
        self.client_sizes.clear();
        self.browser_controller = None;
    }

    fn record_replay(&mut self, bytes: Bytes) {
        record_replay_chunk(&mut self.replay, &mut self.replay_bytes, bytes);
    }

    fn claim_terminal(&mut self) {
        if self.lease.owner != InputLeaseOwner::Terminal {
            self.lease.owner = InputLeaseOwner::Terminal;
            self.bump_lease_epoch();
            self.broadcast_lease();
        }
    }

    fn claim_browser(&mut self, client_id: ClientId) {
        let owner = InputLeaseOwner::Browser(client_id);
        if self.lease.owner != owner {
            self.lease.owner = owner;
            self.bump_lease_epoch();
            self.broadcast_lease();
        }
    }

    fn bump_lease_epoch(&mut self) {
        self.lease.epoch = self.lease.epoch.saturating_add(1);
    }

    fn broadcast_lease(&mut self) {
        let message = self.current_lease_message();
        let mut closed = Vec::new();
        if let Some(output) = &self.terminal
            && output.try_send(message.clone()).is_err()
        {
            self.terminal = None;
        }
        for (client_id, output) in &self.clients {
            if output.try_send(message.clone()).is_err() {
                closed.push(*client_id);
            }
        }
        for client_id in closed {
            self.clients.remove(&client_id);
            self.client_sizes.remove(&client_id);
            if self.browser_controller == Some(client_id) {
                self.browser_controller = None;
            }
        }
    }

    fn broadcast_size(&mut self) {
        let message = self.current_size_message();
        let mut closed = Vec::new();
        for (client_id, output) in &self.clients {
            if output.try_send(message.clone()).is_err() {
                closed.push(*client_id);
            }
        }
        for client_id in closed {
            self.clients.remove(&client_id);
            self.client_sizes.remove(&client_id);
            if self.browser_controller == Some(client_id) {
                self.browser_controller = None;
            }
        }
    }

    fn send_current_lease_to(
        &self,
        output: &ClientOutputTx,
    ) -> Result<(), TrySendError<ClientOutput>> {
        output.try_send(self.current_lease_message())
    }

    fn current_lease_message(&self) -> ClientOutput {
        ClientOutput::Control(ServerControlMessage::LeaseChanged {
            owner: self.lease.owner.protocol_owner(),
            epoch: self.lease.epoch,
        })
    }

    fn current_size_message(&self) -> ClientOutput {
        ClientOutput::Control(ServerControlMessage::SizeChanged {
            size: self.current_size,
        })
    }
}

fn record_replay_chunk(replay: &mut VecDeque<Bytes>, replay_bytes: &mut usize, bytes: Bytes) {
    *replay_bytes = replay_bytes.saturating_add(bytes.len());
    replay.push_back(bytes);
    while *replay_bytes > REPLAY_BUFFER_BYTES || replay.len() > REPLAY_BUFFER_CHUNKS {
        if let Some(removed) = replay.pop_front() {
            *replay_bytes = replay_bytes.saturating_sub(removed.len());
        } else {
            *replay_bytes = 0;
            break;
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
            prepare_tmux_server_environment(&tmux)?;
            ensure_tmux_session(&tmux, session)?;
            prepare_tmux_session_environment(&tmux, session)?;
            prepare_tmux_session_options(&tmux, session)?;
            let mut command = CommandBuilder::new(tmux);
            command.env_remove("TMUX");
            command.arg("-2");
            command.arg("-T");
            command.arg("RGB");
            command.arg("attach-session");
            command.arg("-t");
            command.arg(session.as_str());
            command
        }
    };
    apply_terminal_environment(&mut command);
    slave.spawn_command(command).map_err(RuntimeError::Spawn)
}

fn apply_terminal_environment(command: &mut CommandBuilder) {
    command.env("TERM", TERMINAL_TERM);
    command.env("COLORTERM", TERMINAL_COLOR_MODE);
    command.env("CLICOLOR", "1");
    command.env("TERM_PROGRAM", TERMINAL_PROGRAM);
    command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    for name in DISABLE_COLOR_ENV {
        command.env_remove(name);
    }
}

fn prepare_tmux_server_environment(tmux: &Path) -> Result<(), RuntimeError> {
    if !tmux_server_exists(tmux)? {
        return Ok(());
    }

    for name in DISABLE_COLOR_ENV {
        run_tmux_environment_command(tmux, ["set-environment", "-g", "-u", name])?;
    }
    run_tmux_environment_command(
        tmux,
        ["set-environment", "-g", "COLORTERM", TERMINAL_COLOR_MODE],
    )?;
    run_tmux_environment_command(tmux, ["set-environment", "-g", "CLICOLOR", "1"])?;
    run_tmux_environment_command(
        tmux,
        ["set-environment", "-g", "TERM_PROGRAM", TERMINAL_PROGRAM],
    )?;
    run_tmux_environment_command(
        tmux,
        [
            "set-environment",
            "-g",
            "TERM_PROGRAM_VERSION",
            env!("CARGO_PKG_VERSION"),
        ],
    )?;
    Ok(())
}

fn prepare_tmux_session_environment(
    tmux: &Path,
    session: &SessionName,
) -> Result<(), RuntimeError> {
    for name in DISABLE_COLOR_ENV {
        run_tmux_environment_command(
            tmux,
            ["set-environment", "-t", session.as_str(), "-u", name],
        )?;
    }
    run_tmux_environment_command(
        tmux,
        [
            "set-environment",
            "-t",
            session.as_str(),
            "COLORTERM",
            TERMINAL_COLOR_MODE,
        ],
    )?;
    run_tmux_environment_command(
        tmux,
        ["set-environment", "-t", session.as_str(), "CLICOLOR", "1"],
    )?;
    run_tmux_environment_command(
        tmux,
        [
            "set-environment",
            "-t",
            session.as_str(),
            "TERM_PROGRAM",
            TERMINAL_PROGRAM,
        ],
    )?;
    run_tmux_environment_command(
        tmux,
        [
            "set-environment",
            "-t",
            session.as_str(),
            "TERM_PROGRAM_VERSION",
            env!("CARGO_PKG_VERSION"),
        ],
    )?;
    Ok(())
}

fn ensure_tmux_session(tmux: &Path, session: &SessionName) -> Result<(), RuntimeError> {
    if tmux_session_exists(tmux, session)? {
        return Ok(());
    }

    create_tmux_session(tmux, session)
}

fn prepare_tmux_session_options(tmux: &Path, session: &SessionName) -> Result<(), RuntimeError> {
    run_tmux_option_command(tmux, ["set-option", "-t", session.as_str(), "mouse", "on"])?;
    run_tmux_option_command(
        tmux,
        [
            "set-option",
            "-t",
            session.as_str(),
            "history-limit",
            TMUX_HISTORY_LIMIT,
        ],
    )?;
    Ok(())
}

#[allow(
    clippy::disallowed_types,
    reason = "tmux probes run on the runtime actor's dedicated blocking thread before spawning \
              tmux"
)]
fn tmux_server_exists(tmux: &Path) -> Result<bool, RuntimeError> {
    let status = std::process::Command::new(tmux)
        .env_remove("TMUX")
        .arg("list-sessions")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| RuntimeError::Spawn(error.into()))?;
    Ok(status.success())
}

#[allow(
    clippy::disallowed_types,
    reason = "tmux probes run on the runtime actor's dedicated blocking thread before spawning \
              tmux"
)]
fn tmux_session_exists(tmux: &Path, session: &SessionName) -> Result<bool, RuntimeError> {
    let status = std::process::Command::new(tmux)
        .env_remove("TMUX")
        .arg("has-session")
        .arg("-t")
        .arg(session.as_str())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| RuntimeError::Spawn(error.into()))?;
    Ok(status.success())
}

#[allow(
    clippy::disallowed_types,
    reason = "tmux session creation runs on the runtime actor's dedicated blocking thread before \
              spawning tmux"
)]
fn create_tmux_session(tmux: &Path, session: &SessionName) -> Result<(), RuntimeError> {
    let status = std::process::Command::new(tmux)
        .env_remove("TMUX")
        .env("TERM", TERMINAL_TERM)
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
        .status()
        .map_err(|error| RuntimeError::Spawn(error.into()))?;
    if status.success() {
        Ok(())
    } else {
        Err(RuntimeError::Spawn(anyhow::anyhow!(
            "tmux new-session exited with {status}"
        )))
    }
}

#[allow(
    clippy::disallowed_types,
    reason = "tmux environment setup runs on the runtime actor's dedicated blocking thread before \
              spawning tmux"
)]
fn run_tmux_environment_command<const N: usize>(
    tmux: &Path,
    args: [&str; N],
) -> Result<(), RuntimeError> {
    let status = std::process::Command::new(tmux)
        .env_remove("TMUX")
        .args(args)
        .status()
        .map_err(|error| RuntimeError::Spawn(error.into()))?;
    if status.success() {
        Ok(())
    } else {
        Err(RuntimeError::Spawn(anyhow::anyhow!(
            "tmux environment command exited with {status}"
        )))
    }
}

#[allow(
    clippy::disallowed_types,
    reason = "tmux option setup runs on the runtime actor's dedicated blocking thread before \
              spawning tmux"
)]
fn run_tmux_option_command<const N: usize>(
    tmux: &Path,
    args: [&str; N],
) -> Result<(), RuntimeError> {
    let status = std::process::Command::new(tmux)
        .env_remove("TMUX")
        .args(args)
        .status()
        .map_err(|error| RuntimeError::Spawn(error.into()))?;
    if status.success() {
        Ok(())
    } else {
        Err(RuntimeError::Spawn(anyhow::anyhow!(
            "tmux option command exited with {status}"
        )))
    }
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    pty_tx: mpsc::SyncSender<PtyEvent>,
    generation: u64,
) -> Result<JoinHandle<()>, RuntimeError> {
    thread::Builder::new()
        .name("termstage-pty-reader".to_owned())
        .spawn(move || {
            let mut buffer = [0_u8; PTY_READ_CHUNK_SIZE];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _result = pty_tx.send(PtyEvent {
                            generation,
                            kind: PtyEventKind::Eof,
                        });
                        break;
                    }
                    Ok(read) => {
                        let Some(chunk) = buffer.get(..read) else {
                            let _result = pty_tx.send(PtyEvent {
                                generation,
                                kind: PtyEventKind::ReaderError("invalid pty read size".into()),
                            });
                            break;
                        };
                        if pty_tx
                            .send(PtyEvent {
                                generation,
                                kind: PtyEventKind::Output(Bytes::copy_from_slice(chunk)),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _result = pty_tx.send(PtyEvent {
                            generation,
                            kind: PtyEventKind::ReaderError(error.to_string()),
                        });
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
#[allow(
    clippy::disallowed_types,
    reason = "runtime tests use blocking subprocess probes to validate tmux integration"
)]
mod tests {
    use std::{collections::VecDeque, ffi::OsStr, process::Command, time::Instant};

    use anyhow::Context;
    use tokio::{
        sync::Mutex,
        time::{sleep, timeout},
    };

    static TMUX_TEST_LOCK: Mutex<()> = Mutex::const_new(());

    use super::*;

    fn test_size() -> anyhow::Result<TerminalSize> {
        TerminalSize::new(80, 24).map_err(Into::into)
    }

    fn test_shell_command() -> anyhow::Result<ShellCommand> {
        ShellCommand::new(
            "/bin/bash",
            [OsString::from("--noprofile"), OsString::from("--norc")],
        )
        .map_err(Into::into)
    }

    #[test]
    fn test_should_advertise_truecolor_terminal_environment() {
        let mut command = CommandBuilder::new("/bin/sh");
        command.env("NO_COLOR", "1");
        command.env("ANSI_COLORS_DISABLED", "1");
        apply_terminal_environment(&mut command);

        assert_eq!(command.get_env("TERM"), Some(OsStr::new("xterm-256color")));
        assert_eq!(command.get_env("COLORTERM"), Some(OsStr::new("truecolor")));
        assert_eq!(command.get_env("CLICOLOR"), Some(OsStr::new("1")));
        assert_eq!(
            command.get_env("TERM_PROGRAM"),
            Some(OsStr::new("termstage"))
        );
        assert_eq!(command.get_env("NO_COLOR"), None);
        assert_eq!(command.get_env("ANSI_COLORS_DISABLED"), None);
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

    async fn recv_until_closed(
        output: &mut ClientOutputRx,
        reason: ShutdownReason,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = timeout(remaining, output.recv())
                .await
                .context("test runtime output timed out")?
                .context("client output channel closed")?;
            if message == ClientOutput::Closed(reason.clone()) {
                return Ok(());
            }
        }
        anyhow::bail!("runtime output did not close with requested reason");
    }

    async fn recv_until_process_exited(output: &mut ClientOutputRx) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = timeout(remaining, output.recv())
                .await
                .context("test runtime output timed out")?
                .context("client output channel closed")?;
            if matches!(
                message,
                ClientOutput::Control(ServerControlMessage::ProcessExited { .. })
            ) {
                return Ok(());
            }
        }
        anyhow::bail!("runtime output did not report process exit");
    }

    async fn recv_until_lease(
        output: &mut ClientOutputRx,
        owner: LeaseOwner,
        epoch: u64,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = timeout(remaining, output.recv())
                .await
                .context("test runtime output timed out")?
                .context("client output channel closed")?;
            if matches!(
                message,
                ClientOutput::Control(ServerControlMessage::LeaseChanged {
                    owner: lease_owner,
                    epoch: lease_epoch,
                }) if lease_owner == owner && lease_epoch == epoch
            ) {
                return Ok(());
            }
        }
        anyhow::bail!("runtime output did not receive requested lease state");
    }

    async fn recv_until_replay_finished(
        output: &mut ClientOutputRx,
    ) -> anyhow::Result<Vec<ClientOutput>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut messages = Vec::new();
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = timeout(remaining, output.recv())
                .await
                .context("test runtime output timed out")?
                .context("client output channel closed")?;
            let finished = matches!(
                message,
                ClientOutput::Control(ServerControlMessage::ReplayFinished)
            );
            messages.push(message);
            if finished {
                return Ok(messages);
            }
        }
        anyhow::bail!("runtime output did not finish replay");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_start_shell_attach_input_resize_detach_and_shutdown() -> anyhow::Result<()>
    {
        let config = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
            exit_policy: ExitPolicy::Hold,
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
            [OsString::from("-c"), OsString::from("printf done")],
        )?;
        let config = RuntimeConfig {
            mode: SessionMode::NewShell { shell },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
            exit_policy: ExitPolicy::End,
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
        recv_until_closed(&mut output_rx, ShutdownReason::ChildExit).await?;
        drop(session);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_wrap_browser_replay_with_control_markers() -> anyhow::Result<()> {
        let config = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
            exit_policy: ExitPolicy::Hold,
        };
        let session = RuntimeSession::start(config)?;
        let (first_tx, mut first_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(1),
                output: first_tx,
            })
            .await?;
        recv_until_replay_finished(&mut first_rx).await?;

        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(1),
                bytes: Bytes::from_static(b"printf replay-marker\\n\n"),
            })
            .await?;
        recv_until_contains(&mut first_rx, b"replay-marker").await?;

        let (second_tx, mut second_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(2),
                output: second_tx,
            })
            .await?;
        let messages = recv_until_replay_finished(&mut second_rx).await?;
        assert!(matches!(
            messages.first(),
            Some(ClientOutput::Control(ServerControlMessage::Ready { .. }))
        ));
        assert!(messages.iter().any(|message| matches!(
            message,
            ClientOutput::Control(ServerControlMessage::ReplayStarted)
        )));
        let replay_started_index = messages
            .iter()
            .position(|message| {
                matches!(
                    message,
                    ClientOutput::Control(ServerControlMessage::ReplayStarted)
                )
            })
            .context("missing replay started marker")?;
        let replay_finished_index = messages
            .iter()
            .position(|message| {
                matches!(
                    message,
                    ClientOutput::Control(ServerControlMessage::ReplayFinished)
                )
            })
            .context("missing replay finished marker")?;
        assert!(replay_started_index < replay_finished_index);
        assert!(
            messages[replay_started_index + 1..replay_finished_index]
                .iter()
                .any(|message| matches!(message, ClientOutput::Bytes(bytes) if bytes.windows(b"replay-marker".len()).any(|window| window == b"replay-marker")))
        );
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_transfer_input_lease_between_terminal_and_browser() -> anyhow::Result<()> {
        let config = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
            exit_policy: ExitPolicy::Hold,
        };
        let session = RuntimeSession::start(config)?;
        let (terminal_tx, mut terminal_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachTerminal {
                output: terminal_tx,
            })
            .await?;
        recv_until_lease(&mut terminal_rx, LeaseOwner::Terminal, 0).await?;

        let (browser_tx, mut browser_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(1),
                output: browser_tx,
            })
            .await?;
        let ready = browser_rx.recv().await.context("browser ready output")?;
        assert!(matches!(
            ready,
            ClientOutput::Control(ServerControlMessage::Ready { .. })
        ));
        recv_until_lease(&mut browser_rx, LeaseOwner::Terminal, 0).await?;

        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(1),
                bytes: Bytes::from_static(b"printf browser-lease\\n\n"),
            })
            .await?;
        recv_until_lease(&mut terminal_rx, LeaseOwner::Browser, 1).await?;
        recv_until_lease(&mut browser_rx, LeaseOwner::Browser, 1).await?;
        let browser_output = recv_until_contains(&mut browser_rx, b"browser-lease").await?;
        assert!(
            browser_output
                .windows(b"browser-lease".len())
                .any(|window| window == b"browser-lease")
        );

        session
            .send(RuntimeCommand::TerminalInput {
                bytes: Bytes::from_static(b"printf terminal-lease\\n\n"),
            })
            .await?;
        recv_until_lease(&mut terminal_rx, LeaseOwner::Terminal, 2).await?;
        recv_until_lease(&mut browser_rx, LeaseOwner::Terminal, 2).await?;
        let terminal_output = recv_until_contains(&mut terminal_rx, b"terminal-lease").await?;
        assert!(
            terminal_output
                .windows(b"terminal-lease".len())
                .any(|window| window == b"terminal-lease")
        );
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_ignore_browser_resize_while_terminal_owns_lease() -> anyhow::Result<()> {
        let config = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
            exit_policy: ExitPolicy::Hold,
        };
        let session = RuntimeSession::start(config)?;
        let (terminal_tx, mut terminal_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachTerminal {
                output: terminal_tx,
            })
            .await?;
        recv_until_lease(&mut terminal_rx, LeaseOwner::Terminal, 0).await?;

        let (browser_tx, mut browser_rx) = RuntimeSession::client_mailbox();
        let browser_id = ClientId::new(1);
        session
            .send(RuntimeCommand::AttachClient {
                client_id: browser_id,
                output: browser_tx,
            })
            .await?;
        let ready = browser_rx.recv().await.context("browser ready output")?;
        assert!(matches!(
            ready,
            ClientOutput::Control(ServerControlMessage::Ready { .. })
        ));
        recv_until_lease(&mut browser_rx, LeaseOwner::Terminal, 0).await?;

        session
            .send(RuntimeCommand::BrowserResize {
                client_id: browser_id,
                size: TerminalSize::new(120, 40)?,
            })
            .await?;
        session
            .send(RuntimeCommand::TerminalInput {
                bytes: Bytes::from_static(b"printf 'terminal-size:%s\\n' \"$(stty size)\"\n"),
            })
            .await?;
        let terminal_output = recv_until_contains(&mut terminal_rx, b"terminal-size:24 80").await?;
        assert!(
            terminal_output
                .windows(b"terminal-size:24 80".len())
                .any(|window| window == b"terminal-size:24 80")
        );

        session
            .send(RuntimeCommand::Input {
                client_id: browser_id,
                bytes: Bytes::from_static(b"printf 'browser-size:%s\\n' \"$(stty size)\"\n"),
            })
            .await?;
        recv_until_lease(&mut browser_rx, LeaseOwner::Browser, 1).await?;
        let browser_output = recv_until_contains(&mut browser_rx, b"browser-size:24 80").await?;
        assert!(
            browser_output
                .windows(b"browser-size:24 80".len())
                .any(|window| window == b"browser-size:24 80")
        );

        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_hold_session_after_child_exit() -> anyhow::Result<()> {
        let shell = test_shell_command()?;
        let config = RuntimeConfig {
            mode: SessionMode::NewShell { shell },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
            exit_policy: ExitPolicy::Hold,
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
        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(1),
                bytes: Bytes::from_static(b"exit\n"),
            })
            .await?;
        recv_until_process_exited(&mut output_rx)
            .await
            .context("initial process exit notice")?;

        let (reattach_tx, mut reattach_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(2),
                output: reattach_tx,
            })
            .await?;
        let ready = reattach_rx.recv().await.context("reattach ready output")?;
        assert!(
            matches!(
                ready,
                ClientOutput::Control(ServerControlMessage::Ready { .. })
            ),
            "{ready:?}"
        );
        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(2),
                bytes: Bytes::from_static(b"printf restarted-after-exit\\n\n"),
            })
            .await?;
        let output = recv_until_contains(&mut reattach_rx, b"restarted-after-exit")
            .await
            .context("restart output")?;
        assert!(
            output
                .windows(b"restarted-after-exit".len())
                .any(|window| window == b"restarted-after-exit")
        );
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_start_tmux_mode() -> anyhow::Result<()> {
        let _tmux_guard = TMUX_TEST_LOCK.lock().await;
        let session_name = SessionName::new(format!("termstage-test-{}", std::process::id()))?;
        let config = RuntimeConfig {
            mode: SessionMode::Tmux {
                session: session_name.clone(),
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
            exit_policy: ExitPolicy::Hold,
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

    #[tokio::test(flavor = "current_thread")]
    async fn test_should_prepare_existing_tmux_session_color_environment() -> anyhow::Result<()> {
        let _tmux_guard = TMUX_TEST_LOCK.lock().await;
        let tmux = which::which("tmux").context("tmux unavailable")?;
        let session_name = SessionName::new(format!("termstage-env-test-{}", std::process::id()))?;
        let _cleanup = TmuxSessionCleanup::new(tmux.clone(), session_name.clone());
        let status = Command::new(&tmux)
            .env_remove("TMUX")
            .args([
                "new-session",
                "-d",
                "-s",
                session_name.as_str(),
                "sleep",
                "30",
            ])
            .status()
            .context("failed to create tmux test session")?;
        assert!(status.success());
        set_tmux_test_environment(&tmux, &session_name, "NO_COLOR", "1")?;
        set_tmux_test_environment(&tmux, &session_name, "ANSI_COLORS_DISABLED", "1")?;

        prepare_tmux_session_environment(&tmux, &session_name)?;

        assert_eq!(
            tmux_environment_value(&tmux, &session_name, "NO_COLOR")?,
            None
        );
        assert_eq!(
            tmux_environment_value(&tmux, &session_name, "ANSI_COLORS_DISABLED")?,
            None
        );
        assert_eq!(
            tmux_environment_value(&tmux, &session_name, "COLORTERM")?,
            Some(TERMINAL_COLOR_MODE.to_owned())
        );
        assert_eq!(
            tmux_environment_value(&tmux, &session_name, "CLICOLOR")?,
            Some("1".to_owned())
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_should_prepare_existing_tmux_session_scroll_options() -> anyhow::Result<()> {
        let _tmux_guard = TMUX_TEST_LOCK.lock().await;
        let tmux = which::which("tmux").context("tmux unavailable")?;
        let session_name =
            SessionName::new(format!("termstage-scroll-test-{}", std::process::id()))?;
        let _cleanup = TmuxSessionCleanup::new(tmux.clone(), session_name.clone());
        let status = Command::new(&tmux)
            .env_remove("TMUX")
            .args([
                "new-session",
                "-d",
                "-s",
                session_name.as_str(),
                "sleep",
                "30",
            ])
            .status()
            .context("failed to create tmux test session")?;
        assert!(status.success());
        set_tmux_session_option(&tmux, &session_name, "mouse", "off")?;
        set_tmux_session_option(&tmux, &session_name, "history-limit", "2000")?;

        prepare_tmux_session_options(&tmux, &session_name)?;

        assert_eq!(
            tmux_option_value(&tmux, &session_name, "mouse")?,
            Some("on".to_owned())
        );
        assert_eq!(
            tmux_option_value(&tmux, &session_name, "history-limit")?,
            Some(TMUX_HISTORY_LIMIT.to_owned())
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_should_create_tmux_session_before_attach() -> anyhow::Result<()> {
        let _tmux_guard = TMUX_TEST_LOCK.lock().await;
        let tmux = which::which("tmux").context("tmux unavailable")?;
        let session_name =
            SessionName::new(format!("termstage-ensure-test-{}", std::process::id()))?;
        let _cleanup = TmuxSessionCleanup::new(tmux.clone(), session_name.clone());

        ensure_tmux_session(&tmux, &session_name)?;
        prepare_tmux_session_environment(&tmux, &session_name)?;
        prepare_tmux_session_options(&tmux, &session_name)?;

        assert!(tmux_session_exists(&tmux, &session_name)?);
        assert_eq!(
            tmux_environment_value(&tmux, &session_name, "COLORTERM")?,
            Some(TERMINAL_COLOR_MODE.to_owned())
        );
        assert_eq!(
            tmux_option_value(&tmux, &session_name, "mouse")?,
            Some("on".to_owned())
        );
        assert_eq!(
            tmux_option_value(&tmux, &session_name, "history-limit")?,
            Some(TMUX_HISTORY_LIMIT.to_owned())
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_should_prepare_tmux_server_color_environment() -> anyhow::Result<()> {
        let _tmux_guard = TMUX_TEST_LOCK.lock().await;
        let tmux = which::which("tmux").context("tmux unavailable")?;
        let session_name =
            SessionName::new(format!("termstage-global-env-test-{}", std::process::id()))?;
        let _session_cleanup = TmuxSessionCleanup::new(tmux.clone(), session_name.clone());
        let _environment_cleanup = TmuxGlobalEnvironmentCleanup::capture(
            tmux.clone(),
            [
                "NO_COLOR",
                "ANSI_COLORS_DISABLED",
                "COLORTERM",
                "CLICOLOR",
                "TERM_PROGRAM",
                "TERM_PROGRAM_VERSION",
            ],
        )?;
        let status = Command::new(&tmux)
            .env_remove("TMUX")
            .args([
                "new-session",
                "-d",
                "-s",
                session_name.as_str(),
                "sleep",
                "30",
            ])
            .status()
            .context("failed to create tmux test session")?;
        assert!(status.success());
        set_tmux_global_test_environment(&tmux, "NO_COLOR", "1")?;
        set_tmux_global_test_environment(&tmux, "ANSI_COLORS_DISABLED", "1")?;

        prepare_tmux_server_environment(&tmux)?;

        assert_eq!(tmux_global_environment_value(&tmux, "NO_COLOR")?, None);
        assert_eq!(
            tmux_global_environment_value(&tmux, "ANSI_COLORS_DISABLED")?,
            None
        );
        assert_eq!(
            tmux_global_environment_value(&tmux, "COLORTERM")?,
            Some(TERMINAL_COLOR_MODE.to_owned())
        );
        assert_eq!(
            tmux_global_environment_value(&tmux, "CLICOLOR")?,
            Some("1".to_owned())
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_allow_controller_reattach_after_detach() -> anyhow::Result<()> {
        let config = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
            exit_policy: ExitPolicy::Hold,
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
    async fn test_should_replace_existing_controller_on_new_attach() -> anyhow::Result<()> {
        let config = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
            exit_policy: ExitPolicy::Hold,
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

        let (second_tx, mut second_rx) = RuntimeSession::client_mailbox();
        session
            .send(RuntimeCommand::AttachClient {
                client_id: ClientId::new(2),
                output: second_tx,
            })
            .await?;

        recv_until_closed(&mut first_rx, ShutdownReason::ControllerReplaced).await?;
        let second_ready = second_rx.recv().await.context("second ready output")?;
        assert!(matches!(
            second_ready,
            ClientOutput::Control(ServerControlMessage::Ready { .. })
        ));
        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(1),
                bytes: Bytes::from_static(b"printf should-not-run\\n\n"),
            })
            .await?;
        session
            .send(RuntimeCommand::Input {
                client_id: ClientId::new(2),
                bytes: Bytes::from_static(b"printf replacement-controller-ok\\n\n"),
            })
            .await?;
        let second_output =
            recv_until_contains(&mut second_rx, b"replacement-controller-ok").await?;
        assert!(
            second_output
                .windows(b"replacement-controller-ok".len())
                .any(|window| window == b"replacement-controller-ok")
        );
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[derive(Debug)]
    struct TmuxSessionCleanup {
        tmux: PathBuf,
        session: SessionName,
    }

    impl TmuxSessionCleanup {
        fn new(tmux: PathBuf, session: SessionName) -> Self {
            Self { tmux, session }
        }
    }

    #[derive(Debug)]
    struct TmuxGlobalEnvironmentCleanup {
        tmux: PathBuf,
        values: Vec<(&'static str, Option<String>)>,
    }

    impl TmuxGlobalEnvironmentCleanup {
        fn capture<const N: usize>(
            tmux: PathBuf,
            names: [&'static str; N],
        ) -> anyhow::Result<Self> {
            let mut values = Vec::with_capacity(names.len());
            for name in names {
                values.push((name, tmux_global_environment_value(&tmux, name)?));
            }
            Ok(Self { tmux, values })
        }
    }

    impl Drop for TmuxGlobalEnvironmentCleanup {
        fn drop(&mut self) {
            for (name, value) in &self.values {
                match value {
                    Some(value) => {
                        let _result = Command::new(&self.tmux)
                            .env_remove("TMUX")
                            .args(["set-environment", "-g", name, value])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                    }
                    None => {
                        let _result = Command::new(&self.tmux)
                            .env_remove("TMUX")
                            .args(["set-environment", "-g", "-u", name])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                    }
                }
            }
        }
    }

    impl Drop for TmuxSessionCleanup {
        fn drop(&mut self) {
            let _result = Command::new(&self.tmux)
                .env_remove("TMUX")
                .args(["kill-session", "-t", self.session.as_str()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    fn set_tmux_test_environment(
        tmux: &Path,
        session: &SessionName,
        name: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        let status = Command::new(tmux)
            .env_remove("TMUX")
            .args(["set-environment", "-t", session.as_str(), name, value])
            .status()
            .with_context(|| format!("failed to set tmux env {name}"))?;
        if status.success() {
            Ok(())
        } else {
            anyhow::bail!("tmux set-environment {name} exited with {status}");
        }
    }

    fn set_tmux_global_test_environment(
        tmux: &Path,
        name: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        let status = Command::new(tmux)
            .env_remove("TMUX")
            .args(["set-environment", "-g", name, value])
            .status()
            .with_context(|| format!("failed to set global tmux env {name}"))?;
        if status.success() {
            Ok(())
        } else {
            anyhow::bail!("tmux set-environment -g {name} exited with {status}");
        }
    }

    fn set_tmux_session_option(
        tmux: &Path,
        session: &SessionName,
        name: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        let status = Command::new(tmux)
            .env_remove("TMUX")
            .args(["set-option", "-t", session.as_str(), name, value])
            .status()
            .with_context(|| format!("failed to set tmux option {name}"))?;
        if status.success() {
            Ok(())
        } else {
            anyhow::bail!("tmux set-option {name} exited with {status}");
        }
    }

    fn tmux_environment_value(
        tmux: &Path,
        session: &SessionName,
        name: &str,
    ) -> anyhow::Result<Option<String>> {
        let output = Command::new(tmux)
            .env_remove("TMUX")
            .args(["show-environment", "-t", session.as_str(), name])
            .output()
            .with_context(|| format!("failed to read tmux env {name}"))?;
        if !output.status.success() {
            return Ok(None);
        }
        let line = String::from_utf8(output.stdout).context("tmux env output was not utf-8")?;
        Ok(line
            .trim()
            .strip_prefix(&format!("{name}="))
            .map(ToOwned::to_owned))
    }

    fn tmux_option_value(
        tmux: &Path,
        session: &SessionName,
        name: &str,
    ) -> anyhow::Result<Option<String>> {
        let output = Command::new(tmux)
            .env_remove("TMUX")
            .args(["show-options", "-t", session.as_str(), name])
            .output()
            .with_context(|| format!("failed to read tmux option {name}"))?;
        if !output.status.success() {
            return Ok(None);
        }
        let line = String::from_utf8(output.stdout).context("tmux option output was not utf-8")?;
        Ok(line
            .trim()
            .strip_prefix(&format!("{name} "))
            .map(ToOwned::to_owned))
    }

    fn tmux_global_environment_value(tmux: &Path, name: &str) -> anyhow::Result<Option<String>> {
        let output = Command::new(tmux)
            .env_remove("TMUX")
            .args(["show-environment", "-g", name])
            .output()
            .with_context(|| format!("failed to read global tmux env {name}"))?;
        if !output.status.success() {
            return Ok(None);
        }
        let line = String::from_utf8(output.stdout).context("tmux env output was not utf-8")?;
        Ok(line
            .trim()
            .strip_prefix(&format!("{name}="))
            .map(ToOwned::to_owned))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_replay_recent_output_after_reattach() -> anyhow::Result<()> {
        let config = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
            exit_policy: ExitPolicy::Hold,
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

    #[test]
    fn test_should_bound_replay_by_chunks_with_mailbox_headroom() {
        let total_chunks = CLIENT_OUTPUT_CAPACITY + 25;
        let mut replay = VecDeque::new();
        let mut replay_bytes = 0;

        for index in 0..total_chunks {
            record_replay_chunk(
                &mut replay,
                &mut replay_bytes,
                Bytes::from(format!("chunk-{index:03}")),
            );
        }

        assert_eq!(replay.len(), REPLAY_BUFFER_CHUNKS);
        assert!(replay.len() + 1 < CLIENT_OUTPUT_CAPACITY);
        assert_eq!(replay_bytes, replay.iter().map(Bytes::len).sum::<usize>());
        assert_eq!(
            replay.front(),
            Some(&Bytes::from(format!(
                "chunk-{:03}",
                total_chunks - REPLAY_BUFFER_CHUNKS
            )))
        );
        assert_eq!(
            replay.back(),
            Some(&Bytes::from(format!("chunk-{:03}", total_chunks - 1)))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_close_slow_client_without_stopping_session() -> anyhow::Result<()> {
        let config = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: test_size()?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
            exit_policy: ExitPolicy::Hold,
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
