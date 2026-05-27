//! Runtime tunnel protocol and transport boundaries.
//!
//! The tunnel layer sits between browser/web routing and [`RuntimeSession`].
//! It defines stable semantic frames without making the PTY actor depend on a
//! concrete transport such as WebSocket, TCP, or gRPC.
//!
//! [`RuntimeSession`]: crate::runtime::RuntimeSession

use std::fmt::{self, Debug, Formatter};

use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use tokio::sync::mpsc::{self as tokio_mpsc, error::SendError};

use crate::{
    protocol::{HeartbeatSequence, SafeMessage, ServerControlMessage, SessionName, TerminalSize},
    runtime::{
        ClientId, ClientOutput, ClientOutputRx, RuntimeCommand, RuntimeSession, ShutdownReason,
    },
};

const TUNNEL_TERMINAL_PAYLOAD_MAX_BYTES: usize = 64 * 1024;

/// Tunnel protocol failure.
#[derive(Debug, Error)]
pub enum TunnelError {
    /// A terminal payload exceeded the configured frame cap.
    #[error("tunnel terminal payload exceeds 65536 bytes")]
    TerminalPayloadTooLarge,
    /// JSON tunnel frame encoding or decoding failed.
    #[error("failed to encode or decode tunnel JSON frame")]
    Json(#[source] serde_json::Error),
    /// The payload kind is not supported by this codec.
    #[error("unsupported tunnel payload for codec")]
    UnsupportedPayload,
    /// The tunnel transport closed.
    #[error("tunnel transport closed")]
    TransportClosed,
    /// Sending a runtime command failed because the runtime stopped.
    #[error("runtime command channel closed")]
    RuntimeCommandChannelClosed,
}

/// Bounded terminal bytes carried by tunnel frames.
#[derive(Clone, PartialEq, Eq)]
pub struct TunnelTerminalPayload(Bytes);

impl TunnelTerminalPayload {
    /// Creates a bounded terminal payload.
    ///
    /// # Errors
    ///
    /// Returns [`TunnelError::TerminalPayloadTooLarge`] when the payload exceeds
    /// 64 KiB.
    pub fn new(bytes: Bytes) -> Result<Self, TunnelError> {
        if bytes.len() <= TUNNEL_TERMINAL_PAYLOAD_MAX_BYTES {
            Ok(Self(bytes))
        } else {
            Err(TunnelError::TerminalPayloadTooLarge)
        }
    }

    /// Returns the payload length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether the payload is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the payload bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_ref()
    }

    /// Consumes the payload and returns the underlying bytes.
    #[must_use]
    pub fn into_bytes(self) -> Bytes {
        self.0
    }
}

impl Debug for TunnelTerminalPayload {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TunnelTerminalPayload")
            .field("len", &self.0.len())
            .finish()
    }
}

impl Serialize for TunnelTerminalPayload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for TunnelTerminalPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        Self::new(Bytes::from(bytes)).map_err(serde::de::Error::custom)
    }
}

/// Transport-level payload passed to codecs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelPayload {
    /// Text transport payload, usually JSON control data.
    Text(String),
    /// Binary transport payload, usually raw terminal bytes with an envelope.
    Binary(Bytes),
}

/// Stable close reason carried through the runtime tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "reason", content = "detail", rename_all = "camelCase")]
pub enum TunnelCloseReason {
    /// Server supervisor requested shutdown.
    Supervisor,
    /// The browser/client disconnected.
    ClientDisconnect,
    /// A newer browser/client took over the controller role.
    ControllerReplaced,
    /// The child process exited or the PTY reached EOF.
    ChildExit,
    /// Runtime error.
    RuntimeError(SafeMessage),
}

impl TunnelCloseReason {
    fn from_shutdown_reason(reason: ShutdownReason) -> Self {
        match reason {
            ShutdownReason::Supervisor => Self::Supervisor,
            ShutdownReason::ClientDisconnect => Self::ClientDisconnect,
            ShutdownReason::ControllerReplaced => Self::ControllerReplaced,
            ShutdownReason::ChildExit => Self::ChildExit,
            ShutdownReason::RuntimeError(message) => {
                let message = SafeMessage::new(message)
                    .unwrap_or_else(|_error| SafeMessage::from_static("runtime error"));
                Self::RuntimeError(message)
            }
        }
    }
}

/// Runtime-side control frame.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum TunnelRuntimeControl {
    /// Server-to-browser runtime control message.
    Server {
        /// Runtime control message.
        message: ServerControlMessage,
    },
    /// Runtime closed the client stream.
    Closed {
        /// Stable close reason.
        reason: TunnelCloseReason,
    },
}

/// Stable semantic frame exchanged between web side and runtime side.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum TunnelFrame {
    /// Runtime side announces a session.
    RegisterSession {
        /// Runtime session id.
        session: SessionName,
        /// Command or shell display text.
        command_display: SafeMessage,
        /// Initial runtime terminal size.
        size: TerminalSize,
    },
    /// Web side attaches a browser client to the runtime side.
    AttachBrowser {
        /// Browser client id.
        client_id: ClientId,
    },
    /// Web side detaches a browser client from the runtime side.
    DetachBrowser {
        /// Browser client id.
        client_id: ClientId,
    },
    /// Web side forwards browser terminal input bytes to the runtime side.
    BrowserInput {
        /// Browser client id.
        client_id: ClientId,
        /// Terminal bytes.
        bytes: TunnelTerminalPayload,
    },
    /// Web side forwards browser terminal dimensions to the runtime side.
    BrowserResize {
        /// Browser client id.
        client_id: ClientId,
        /// Proposed terminal size.
        size: TerminalSize,
    },
    /// Runtime side forwards PTY output bytes to the web side.
    PtyOutput {
        /// Terminal bytes.
        bytes: TunnelTerminalPayload,
    },
    /// Runtime side forwards runtime control to the web side.
    RuntimeControl {
        /// Browser client id when the control is scoped to one client.
        client_id: Option<ClientId>,
        /// Runtime control message.
        control: TunnelRuntimeControl,
    },
    /// Liveness heartbeat.
    Heartbeat {
        /// Heartbeat sequence.
        sequence: HeartbeatSequence,
    },
}

/// Codec for converting semantic tunnel frames to transport payloads.
pub trait TunnelCodec: Debug + Send + Sync {
    /// Encodes a semantic tunnel frame.
    ///
    /// # Errors
    ///
    /// Returns [`TunnelError`] when the frame cannot be encoded.
    fn encode(&self, frame: &TunnelFrame) -> Result<TunnelPayload, TunnelError>;

    /// Decodes a semantic tunnel frame.
    ///
    /// # Errors
    ///
    /// Returns [`TunnelError`] when the payload cannot be decoded.
    fn decode(&self, payload: TunnelPayload) -> Result<TunnelFrame, TunnelError>;
}

/// JSON codec for tunnel control frames and test fixtures.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonTunnelCodec;

impl TunnelCodec for JsonTunnelCodec {
    fn encode(&self, frame: &TunnelFrame) -> Result<TunnelPayload, TunnelError> {
        serde_json::to_string(frame)
            .map(TunnelPayload::Text)
            .map_err(TunnelError::Json)
    }

    fn decode(&self, payload: TunnelPayload) -> Result<TunnelFrame, TunnelError> {
        match payload {
            TunnelPayload::Text(text) => serde_json::from_str(&text).map_err(TunnelError::Json),
            TunnelPayload::Binary(_bytes) => Err(TunnelError::UnsupportedPayload),
        }
    }
}

// Native async trait methods keep transport implementations readable and match
// AGENTS.md guidance. This trait is crate-owned and not used for dyn dispatch.
#[allow(async_fn_in_trait)]
/// Boundary for concrete tunnel transports.
pub trait TunnelTransport: Debug + Send {
    /// Sends one semantic frame through the transport.
    ///
    /// # Errors
    ///
    /// Returns [`TunnelError`] when the transport cannot send the frame.
    async fn send_frame(&mut self, frame: TunnelFrame) -> Result<(), TunnelError>;

    /// Receives one semantic frame from the transport.
    ///
    /// # Errors
    ///
    /// Returns [`TunnelError`] when the transport cannot decode or receive a
    /// frame.
    async fn receive_frame(&mut self) -> Result<Option<TunnelFrame>, TunnelError>;
}

/// Runtime-side action produced from a tunnel frame.
#[derive(Debug)]
pub enum RuntimeTunnelAction {
    /// Attach a browser. The caller must create the runtime output mailbox.
    AttachBrowser {
        /// Browser client id.
        client_id: ClientId,
    },
    /// Send a runtime command directly.
    Command(RuntimeCommand),
    /// No runtime command is needed.
    None,
}

/// Bridge mapping between tunnel frames and runtime commands/output.
#[derive(Debug, Clone, Copy, Default)]
pub struct RuntimeTunnelBridge;

/// Runtime tunnel bridge completion reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeTunnelBridgeOutcome {
    /// The transport closed cleanly.
    TransportClosed,
    /// The runtime output stream closed.
    RuntimeStopped,
}

#[derive(Debug)]
struct AttachedRuntimeClient {
    client_id: ClientId,
    output: ClientOutputRx,
}

impl RuntimeTunnelBridge {
    /// Runs the bridge loop between one tunnel transport and a runtime session.
    ///
    /// # Errors
    ///
    /// Returns [`TunnelError`] when transport IO fails, a tunnel payload is
    /// invalid, or the runtime command channel has stopped.
    pub async fn run<T>(
        mut transport: T,
        commands: tokio_mpsc::Sender<RuntimeCommand>,
    ) -> Result<RuntimeTunnelBridgeOutcome, TunnelError>
    where
        T: TunnelTransport,
    {
        let mut attached: Option<AttachedRuntimeClient> = None;
        loop {
            let outcome = if let Some(mut client) = attached.take() {
                tokio::select! {
                    frame = transport.receive_frame() => {
                        attached = Some(client);
                        Self::handle_transport_frame(frame?, &commands, &mut attached).await?
                    }
                    output = client.output.recv() => {
                        let client_id = client.client_id;
                        attached = Some(client);
                        Self::handle_runtime_output(output, client_id, &mut transport).await?
                    }
                }
            } else {
                let frame = transport.receive_frame().await?;
                Self::handle_transport_frame(frame, &commands, &mut attached).await?
            };

            if let Some(outcome) = outcome {
                return Ok(outcome);
            }
        }
    }

    async fn handle_transport_frame(
        frame: Option<TunnelFrame>,
        commands: &tokio_mpsc::Sender<RuntimeCommand>,
        attached: &mut Option<AttachedRuntimeClient>,
    ) -> Result<Option<RuntimeTunnelBridgeOutcome>, TunnelError> {
        let Some(frame) = frame else {
            if let Some(client) = attached.take() {
                Self::send_runtime_command(
                    commands,
                    RuntimeCommand::DetachClient {
                        client_id: client.client_id,
                    },
                )
                .await?;
            }
            return Ok(Some(RuntimeTunnelBridgeOutcome::TransportClosed));
        };

        match Self::action_from_frame(frame) {
            RuntimeTunnelAction::AttachBrowser { client_id } => {
                let (output_tx, output_rx) = RuntimeSession::client_mailbox();
                Self::send_runtime_command(commands, Self::attach_command(client_id, output_tx))
                    .await?;
                *attached = Some(AttachedRuntimeClient {
                    client_id,
                    output: output_rx,
                });
            }
            RuntimeTunnelAction::Command(command) => {
                if let RuntimeCommand::DetachClient { client_id } = command
                    && attached
                        .as_ref()
                        .is_some_and(|client| client.client_id == client_id)
                {
                    *attached = None;
                }
                Self::send_runtime_command(commands, command).await?;
            }
            RuntimeTunnelAction::None => {}
        }
        Ok(None)
    }

    async fn handle_runtime_output<T>(
        output: Option<ClientOutput>,
        client_id: ClientId,
        transport: &mut T,
    ) -> Result<Option<RuntimeTunnelBridgeOutcome>, TunnelError>
    where
        T: TunnelTransport,
    {
        let Some(output) = output else {
            return Ok(Some(RuntimeTunnelBridgeOutcome::RuntimeStopped));
        };
        let is_closed = matches!(output, ClientOutput::Closed(_));
        let frame = Self::frame_from_output(Some(client_id), output)?;
        transport.send_frame(frame).await?;
        if is_closed {
            Ok(Some(RuntimeTunnelBridgeOutcome::RuntimeStopped))
        } else {
            Ok(None)
        }
    }

    async fn send_runtime_command(
        commands: &tokio_mpsc::Sender<RuntimeCommand>,
        command: RuntimeCommand,
    ) -> Result<(), TunnelError> {
        commands
            .send(command)
            .await
            .map_err(|_error: SendError<RuntimeCommand>| TunnelError::RuntimeCommandChannelClosed)
    }

    /// Converts a tunnel frame into a runtime-side action.
    #[must_use]
    pub fn action_from_frame(frame: TunnelFrame) -> RuntimeTunnelAction {
        match frame {
            TunnelFrame::AttachBrowser { client_id } => {
                RuntimeTunnelAction::AttachBrowser { client_id }
            }
            TunnelFrame::DetachBrowser { client_id } => {
                RuntimeTunnelAction::Command(RuntimeCommand::DetachClient { client_id })
            }
            TunnelFrame::BrowserInput { client_id, bytes } => {
                RuntimeTunnelAction::Command(RuntimeCommand::Input {
                    client_id,
                    bytes: bytes.into_bytes(),
                })
            }
            TunnelFrame::BrowserResize { client_id, size } => {
                RuntimeTunnelAction::Command(RuntimeCommand::BrowserResize { client_id, size })
            }
            TunnelFrame::RegisterSession { .. }
            | TunnelFrame::PtyOutput { .. }
            | TunnelFrame::RuntimeControl { .. }
            | TunnelFrame::Heartbeat { .. } => RuntimeTunnelAction::None,
        }
    }

    /// Creates an attach command once the bridge has allocated an output
    /// mailbox.
    #[must_use]
    pub fn attach_command(
        client_id: ClientId,
        output: crate::runtime::ClientOutputTx,
    ) -> RuntimeCommand {
        RuntimeCommand::AttachClient { client_id, output }
    }

    /// Converts runtime output into a tunnel frame.
    ///
    /// # Errors
    ///
    /// Returns [`TunnelError::TerminalPayloadTooLarge`] if a runtime byte chunk
    /// exceeds the tunnel frame cap.
    pub fn frame_from_output(
        client_id: Option<ClientId>,
        output: ClientOutput,
    ) -> Result<TunnelFrame, TunnelError> {
        match output {
            ClientOutput::Bytes(bytes) => Ok(TunnelFrame::PtyOutput {
                bytes: TunnelTerminalPayload::new(bytes)?,
            }),
            ClientOutput::Control(message) => Ok(TunnelFrame::RuntimeControl {
                client_id,
                control: TunnelRuntimeControl::Server { message },
            }),
            ClientOutput::Closed(reason) => Ok(TunnelFrame::RuntimeControl {
                client_id,
                control: TunnelRuntimeControl::Closed {
                    reason: TunnelCloseReason::from_shutdown_reason(reason),
                },
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{LeaseOwner, ServerControlMessage};

    #[derive(Debug)]
    struct MemoryTunnelTransport {
        inbound: tokio_mpsc::Receiver<TunnelFrame>,
        outbound: tokio_mpsc::Sender<TunnelFrame>,
    }

    impl TunnelTransport for MemoryTunnelTransport {
        async fn send_frame(&mut self, frame: TunnelFrame) -> Result<(), TunnelError> {
            self.outbound
                .send(frame)
                .await
                .map_err(|_error| TunnelError::TransportClosed)
        }

        async fn receive_frame(&mut self) -> Result<Option<TunnelFrame>, TunnelError> {
            Ok(self.inbound.recv().await)
        }
    }

    #[test]
    fn test_should_round_trip_json_tunnel_frame() -> anyhow::Result<()> {
        let codec = JsonTunnelCodec;
        let frame = TunnelFrame::BrowserResize {
            client_id: ClientId::new(7),
            size: TerminalSize::new(120, 40)?,
        };

        let payload = codec.encode(&frame)?;
        let decoded = codec.decode(payload)?;

        assert_eq!(decoded, frame);
        Ok(())
    }

    #[test]
    fn test_should_reject_oversized_terminal_payload() {
        let bytes = Bytes::from(vec![b'x'; TUNNEL_TERMINAL_PAYLOAD_MAX_BYTES + 1]);

        assert!(matches!(
            TunnelTerminalPayload::new(bytes),
            Err(TunnelError::TerminalPayloadTooLarge)
        ));
    }

    #[test]
    fn test_should_map_browser_input_to_runtime_command() -> anyhow::Result<()> {
        let client_id = ClientId::new(3);
        let frame = TunnelFrame::BrowserInput {
            client_id,
            bytes: TunnelTerminalPayload::new(Bytes::from_static(b"pwd\n"))?,
        };

        let RuntimeTunnelAction::Command(RuntimeCommand::Input {
            client_id: id,
            bytes,
        }) = RuntimeTunnelBridge::action_from_frame(frame)
        else {
            anyhow::bail!("expected runtime input command");
        };

        assert_eq!(id, client_id);
        assert_eq!(bytes, Bytes::from_static(b"pwd\n"));
        Ok(())
    }

    #[test]
    fn test_should_map_runtime_control_to_tunnel_frame() -> anyhow::Result<()> {
        let frame = RuntimeTunnelBridge::frame_from_output(
            Some(ClientId::new(9)),
            ClientOutput::Control(ServerControlMessage::LeaseChanged {
                owner: LeaseOwner::Browser,
                epoch: 4,
            }),
        )?;

        assert_eq!(
            frame,
            TunnelFrame::RuntimeControl {
                client_id: Some(ClientId::new(9)),
                control: TunnelRuntimeControl::Server {
                    message: ServerControlMessage::LeaseChanged {
                        owner: LeaseOwner::Browser,
                        epoch: 4,
                    },
                },
            }
        );
        Ok(())
    }

    #[test]
    fn test_should_map_runtime_close_to_safe_tunnel_reason() -> anyhow::Result<()> {
        let frame = RuntimeTunnelBridge::frame_from_output(
            None,
            ClientOutput::Closed(ShutdownReason::RuntimeError("x".repeat(600))),
        )?;

        assert_eq!(
            frame,
            TunnelFrame::RuntimeControl {
                client_id: None,
                control: TunnelRuntimeControl::Closed {
                    reason: TunnelCloseReason::RuntimeError(SafeMessage::from_static(
                        "runtime error"
                    )),
                },
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_should_run_bridge_attach_input_output_and_detach() -> anyhow::Result<()> {
        let client_id = ClientId::new(11);
        let (inbound_tx, inbound_rx) = tokio_mpsc::channel(8);
        let (outbound_tx, mut outbound_rx) = tokio_mpsc::channel(8);
        let (command_tx, mut command_rx) = tokio_mpsc::channel(8);
        let transport = MemoryTunnelTransport {
            inbound: inbound_rx,
            outbound: outbound_tx,
        };
        let bridge = tokio::spawn(RuntimeTunnelBridge::run(transport, command_tx));

        inbound_tx
            .send(TunnelFrame::AttachBrowser { client_id })
            .await?;
        let Some(RuntimeCommand::AttachClient {
            client_id: attached_id,
            output,
        }) = command_rx.recv().await
        else {
            anyhow::bail!("expected attach command");
        };
        assert_eq!(attached_id, client_id);

        output
            .send(ClientOutput::Bytes(Bytes::from_static(b"runtime-output")))
            .await?;
        let Some(TunnelFrame::PtyOutput { bytes }) = outbound_rx.recv().await else {
            anyhow::bail!("expected pty output frame");
        };
        assert_eq!(bytes.as_bytes(), b"runtime-output");

        inbound_tx
            .send(TunnelFrame::BrowserInput {
                client_id,
                bytes: TunnelTerminalPayload::new(Bytes::from_static(b"browser-input"))?,
            })
            .await?;
        let Some(RuntimeCommand::Input {
            client_id: input_id,
            bytes,
        }) = command_rx.recv().await
        else {
            anyhow::bail!("expected input command");
        };
        assert_eq!(input_id, client_id);
        assert_eq!(bytes, Bytes::from_static(b"browser-input"));

        drop(inbound_tx);
        let outcome = bridge.await??;
        assert_eq!(outcome, RuntimeTunnelBridgeOutcome::TransportClosed);
        let Some(RuntimeCommand::DetachClient {
            client_id: detached_id,
        }) = command_rx.recv().await
        else {
            anyhow::bail!("expected detach command");
        };
        assert_eq!(detached_id, client_id);
        Ok(())
    }

    #[tokio::test]
    async fn test_should_stop_bridge_after_runtime_closed_output() -> anyhow::Result<()> {
        let client_id = ClientId::new(12);
        let (inbound_tx, inbound_rx) = tokio_mpsc::channel(8);
        let (outbound_tx, mut outbound_rx) = tokio_mpsc::channel(8);
        let (command_tx, mut command_rx) = tokio_mpsc::channel(8);
        let transport = MemoryTunnelTransport {
            inbound: inbound_rx,
            outbound: outbound_tx,
        };
        let bridge = tokio::spawn(RuntimeTunnelBridge::run(transport, command_tx));

        inbound_tx
            .send(TunnelFrame::AttachBrowser { client_id })
            .await?;
        let Some(RuntimeCommand::AttachClient { output, .. }) = command_rx.recv().await else {
            anyhow::bail!("expected attach command");
        };
        output
            .send(ClientOutput::Closed(ShutdownReason::ChildExit))
            .await?;

        let Some(TunnelFrame::RuntimeControl { control, .. }) = outbound_rx.recv().await else {
            anyhow::bail!("expected runtime control frame");
        };
        assert_eq!(
            control,
            TunnelRuntimeControl::Closed {
                reason: TunnelCloseReason::ChildExit,
            }
        );
        let outcome = bridge.await??;
        assert_eq!(outcome, RuntimeTunnelBridgeOutcome::RuntimeStopped);
        Ok(())
    }
}
