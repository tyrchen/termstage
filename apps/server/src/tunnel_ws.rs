//! Axum WebSocket transport for runtime tunnel frames.

use axum::extract::ws::{Message, WebSocket};
use futures_util::StreamExt;
use termstage_core::tunnel::{
    JsonTunnelCodec, TunnelCodec, TunnelError, TunnelFrame, TunnelPayload, TunnelTransport,
};

/// Runtime tunnel transport backed by an Axum WebSocket.
#[derive(Debug)]
pub struct AxumWebSocketTunnelTransport<C = JsonTunnelCodec> {
    socket: WebSocket,
    codec: C,
}

impl<C> AxumWebSocketTunnelTransport<C>
where
    C: TunnelCodec,
{
    /// Creates a tunnel transport from an upgraded Axum WebSocket.
    #[must_use]
    pub const fn new(socket: WebSocket, codec: C) -> Self {
        Self { socket, codec }
    }
}

impl<C> TunnelTransport for AxumWebSocketTunnelTransport<C>
where
    C: TunnelCodec,
{
    async fn send_frame(&mut self, frame: TunnelFrame) -> Result<(), TunnelError> {
        let message = match self.codec.encode(&frame)? {
            TunnelPayload::Text(text) => Message::Text(text.into()),
            TunnelPayload::Binary(bytes) => Message::Binary(bytes),
        };
        self.socket
            .send(message)
            .await
            .map_err(|_error| TunnelError::TransportClosed)
    }

    async fn receive_frame(&mut self) -> Result<Option<TunnelFrame>, TunnelError> {
        while let Some(message) = self.socket.next().await {
            match message.map_err(|_error| TunnelError::TransportClosed)? {
                Message::Text(text) => {
                    return self
                        .codec
                        .decode(TunnelPayload::Text(text.to_string()))
                        .map(Some);
                }
                Message::Binary(bytes) => {
                    return self.codec.decode(TunnelPayload::Binary(bytes)).map(Some);
                }
                Message::Ping(bytes) => {
                    self.socket
                        .send(Message::Pong(bytes))
                        .await
                        .map_err(|_error| TunnelError::TransportClosed)?;
                }
                Message::Pong(_bytes) => {}
                Message::Close(_frame) => return Ok(None),
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc};

    use axum::{
        Router,
        extract::{State, ws::WebSocketUpgrade},
        response::IntoResponse,
        routing::get,
    };
    use futures_util::{SinkExt, StreamExt};
    use termstage_core::{
        protocol::{HeartbeatSequence, TerminalSize},
        runtime::ClientId,
        tunnel::TunnelTransport,
    };
    use tokio::{net::TcpListener, sync::Mutex};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Bytes as TungsteniteBytes, Message as TungsteniteMessage},
    };

    use super::*;

    #[derive(Debug, Clone)]
    struct TestState {
        received: Arc<Mutex<Vec<TunnelFrame>>>,
    }

    async fn tunnel_handler(
        State(state): State<TestState>,
        upgrade: WebSocketUpgrade,
    ) -> impl IntoResponse {
        upgrade.on_upgrade(move |socket| async move {
            let mut transport = AxumWebSocketTunnelTransport::new(socket, JsonTunnelCodec);
            let Ok(Some(frame)) = transport.receive_frame().await else {
                return;
            };
            state.received.lock().await.push(frame);
            let _result = transport
                .send_frame(TunnelFrame::Heartbeat {
                    sequence: HeartbeatSequence::new(9),
                })
                .await;
        })
    }

    #[tokio::test]
    async fn test_should_exchange_tunnel_frame_over_axum_websocket() -> anyhow::Result<()> {
        let received = Arc::new(Mutex::new(Vec::new()));
        let state = TestState {
            received: Arc::clone(&received),
        };
        let app = Router::new()
            .route("/tunnel/ws", get(tunnel_handler))
            .with_state(state);
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let (mut socket, _response) = connect_async(format!("ws://{address}/tunnel/ws")).await?;
        let codec = JsonTunnelCodec;
        let frame = TunnelFrame::BrowserResize {
            client_id: ClientId::new(17),
            size: TerminalSize::new(100, 30)?,
        };
        let TunnelPayload::Text(text) = codec.encode(&frame)? else {
            anyhow::bail!("expected text payload");
        };

        socket.send(TungsteniteMessage::Text(text.into())).await?;
        let Some(message) = socket.next().await else {
            anyhow::bail!("expected response frame");
        };
        let TungsteniteMessage::Text(text) = message? else {
            anyhow::bail!("expected text response");
        };
        let decoded = codec.decode(TunnelPayload::Text(text.to_string()))?;

        assert_eq!(
            decoded,
            TunnelFrame::Heartbeat {
                sequence: HeartbeatSequence::new(9),
            }
        );
        assert_eq!(received.lock().await.as_slice(), &[frame]);
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_should_reject_binary_frame_for_json_codec() -> anyhow::Result<()> {
        let received = Arc::new(Mutex::new(Vec::new()));
        let state = TestState {
            received: Arc::clone(&received),
        };
        let app = Router::new()
            .route("/tunnel/ws", get(tunnel_handler))
            .with_state(state);
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let (mut socket, _response) = connect_async(format!("ws://{address}/tunnel/ws")).await?;
        socket
            .send(TungsteniteMessage::Binary(TungsteniteBytes::from_static(
                b"not-json",
            )))
            .await?;

        let close = socket.next().await;
        assert!(matches!(
            close,
            None | Some(Ok(TungsteniteMessage::Close(_)) | Err(_))
        ));
        assert!(received.lock().await.is_empty());
        server.abort();
        Ok(())
    }
}
