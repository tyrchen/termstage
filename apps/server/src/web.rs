//! Axum routes and WebSocket bridge for browser terminal mode.

use std::{
    fmt::{self, Debug, Formatter},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::Context;
use axum::{
    Router,
    extract::{
        ConnectInfo, Path, Query, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use termstage_core::{
    protocol::{
        AccessToken, ClientControlMessage, ErrorCode, SafeMessage, ServerControlMessage,
        TerminalSize,
    },
    runtime::{ClientId, ClientOutput, RuntimeCommand, RuntimeConfig, RuntimeSession},
    security::{
        AllowedHost, AllowedOrigin, BasePath, ExposurePolicy, PublicBaseUrl, SecurityError,
        validate_access_token, validate_peer_for_policy,
    },
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tracing::debug;

use crate::assets::{asset_response, index_response};

const DEFAULT_BIND_HOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const MAX_FRAME_SIZE: usize = 16 * 1024;
const MAX_MESSAGE_SIZE: usize = 64 * 1024;

/// Presentation theme sent to the frontend through the HTML document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationTheme {
    /// High-contrast dark presentation theme.
    HighContrast,
    /// Light presentation theme.
    Light,
}

impl PresentationTheme {
    /// Returns the stable browser value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HighContrast => "high-contrast",
            Self::Light => "light",
        }
    }
}

/// Browser presentation settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationSettings {
    /// Terminal font size in CSS pixels.
    pub font_size: u16,
    /// Terminal color theme.
    pub theme: PresentationTheme,
}

impl Default for PresentationSettings {
    fn default() -> Self {
        Self {
            font_size: 24,
            theme: PresentationTheme::HighContrast,
        }
    }
}

/// Web server configuration.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Bind host.
    pub host: IpAddr,
    /// TCP port. `0` lets the OS choose a free port.
    pub port: u16,
    /// Browser terminal exposure mode.
    pub exposure: WebExposure,
    /// Per-server access token.
    pub token: AccessToken,
    /// Runtime command sender.
    pub commands: mpsc::Sender<RuntimeCommand>,
    /// Runtime session configuration.
    pub runtime: RuntimeConfig,
    /// Browser presentation settings.
    pub presentation: PresentationSettings,
    /// Optional reverse-proxy base path. When set, all routes mount under it.
    pub base_path: Option<BasePath>,
}

impl WebConfig {
    /// Creates a config that binds to `127.0.0.1:0`.
    #[must_use]
    pub fn local(
        token: AccessToken,
        commands: mpsc::Sender<RuntimeCommand>,
        runtime: RuntimeConfig,
    ) -> Self {
        Self {
            host: DEFAULT_BIND_HOST,
            port: 0,
            exposure: WebExposure::Local,
            token,
            commands,
            runtime,
            presentation: PresentationSettings::default(),
            base_path: None,
        }
    }
}

/// Browser terminal exposure mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebExposure {
    /// Local loopback-only service.
    Local,
    /// Public service behind an HTTPS ingress or reverse proxy.
    Public {
        /// Browser-visible public base URL.
        public_url: PublicBaseUrl,
    },
}

/// Running web server.
pub struct RunningServer {
    address: SocketAddr,
    token: AccessToken,
    presentation: PresentationSettings,
    exposure: WebExposure,
    base_path: Option<BasePath>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<anyhow::Result<()>>,
}

impl Debug for RunningServer {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunningServer")
            .field("address", &self.address)
            .field("token", &self.token)
            .field("presentation", &self.presentation)
            .field("exposure", &self.exposure)
            .field("base_path", &self.base_path)
            .finish_non_exhaustive()
    }
}

impl RunningServer {
    /// Returns the bound socket address.
    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Returns the explicit browser launch URL.
    #[must_use]
    pub fn launch_url(&self) -> String {
        match &self.exposure {
            WebExposure::Local => {
                let prefix = self.base_path.as_ref().map_or("/", BasePath::as_str);
                format!(
                    "http://{}{}?token={}&fontSize={}&theme={}",
                    self.address,
                    prefix,
                    self.token.to_url_token(),
                    self.presentation.font_size,
                    self.presentation.theme.as_str()
                )
            }
            WebExposure::Public { public_url } => public_url.launch_url_with_base_path(
                &self.token,
                self.presentation.font_size,
                self.presentation.theme.as_str(),
                self.base_path.as_ref(),
            ),
        }
    }

    /// Requests server shutdown and waits for the serving task to finish.
    ///
    /// # Errors
    ///
    /// Returns an error when the server task panics.
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _result = shutdown.send(());
        }
        self.task
            .await
            .context("browser terminal server task panicked")
            .and_then(std::convert::identity)
    }

    #[cfg(test)]
    fn for_test(
        address: SocketAddr,
        token: AccessToken,
        presentation: PresentationSettings,
    ) -> Self {
        let (shutdown, _shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async { Ok(()) });
        Self {
            address,
            token,
            presentation,
            exposure: WebExposure::Local,
            base_path: None,
            shutdown: Some(shutdown),
            task,
        }
    }
}

#[derive(Clone)]
struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    exposure: ExposurePolicy,
    token: AccessToken,
    commands: mpsc::Sender<RuntimeCommand>,
    runtime: RuntimeConfig,
    presentation: PresentationSettings,
    base_path: Option<BasePath>,
    next_client_id: AtomicU64,
}

impl Debug for AppState {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppState")
            .field("exposure", &self.inner.exposure)
            .field("token", &self.inner.token)
            .field("runtime", &self.inner.runtime)
            .field("presentation", &self.inner.presentation)
            .field("base_path", &self.inner.base_path)
            .finish_non_exhaustive()
    }
}

impl AppState {
    fn new(config: WebConfig) -> Result<Self, SecurityError> {
        let exposure = match config.exposure {
            WebExposure::Local => ExposurePolicy::local(config.host, config.port)?,
            WebExposure::Public { public_url } => ExposurePolicy::Public(public_url),
        };
        Ok(Self {
            inner: Arc::new(AppStateInner {
                exposure,
                token: config.token,
                commands: config.commands,
                runtime: config.runtime,
                presentation: config.presentation,
                base_path: config.base_path,
                next_client_id: AtomicU64::new(1),
            }),
        })
    }

    fn client_id(&self) -> ClientId {
        let id = self.inner.next_client_id.fetch_add(1, Ordering::Relaxed);
        ClientId::new(id)
    }
}

/// Builds the browser terminal router.
///
/// # Errors
///
/// Returns [`SecurityError`] if the configured exposure policy is invalid.
pub fn router(config: WebConfig) -> Result<Router, SecurityError> {
    let base_path = config.base_path.clone();
    let state = AppState::new(config)?;
    let prefix = base_path.as_ref().map_or("", BasePath::nest_prefix);
    let mut router = Router::new()
        .route(&format!("{prefix}/"), get(index))
        .route(&format!("{prefix}/assets/{{*path}}"), get(asset))
        .route(&format!("{prefix}/ws"), get(ws))
        .route(&format!("{prefix}/healthz"), get(healthz));
    if !prefix.is_empty() {
        router = router.route(prefix, get(index));
    }
    Ok(router.with_state(state))
}

/// Starts the Axum server.
///
/// # Errors
///
/// Returns an error when binding, route construction, or serving fails.
pub async fn serve(config: WebConfig) -> anyhow::Result<RunningServer> {
    let listener = TcpListener::bind(SocketAddr::from((config.host, config.port)))
        .await
        .with_context(|| format!("failed to bind browser terminal server on {}", config.host))?;
    let address = listener
        .local_addr()
        .context("failed to read bound browser terminal address")?;
    let token = config.token.clone();
    let presentation = config.presentation;
    let exposure = config.exposure.clone();
    let base_path = config.base_path.clone();
    let app = router(WebConfig {
        port: address.port(),
        ..config
    })
    .context("failed to build browser terminal router")?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _result = shutdown_rx.await;
        })
        .await
        .context("browser terminal server failed")
    });
    Ok(RunningServer {
        address,
        token,
        presentation,
        exposure,
        base_path,
        shutdown: Some(shutdown_tx),
        task,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenQuery {
    token: String,
}

async fn index(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Result<Response, WebError> {
    validate_http_request(&state, peer, &headers, &query, false)?;
    Ok(index_response(state.inner.base_path.as_ref()))
}

async fn asset(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Result<Response, WebError> {
    validate_asset_request(&state, peer, &headers)?;
    Ok(asset_response(&path))
}

async fn healthz() -> &'static str {
    "ok"
}

async fn ws(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, WebError> {
    validate_http_request(&state, peer, &headers, &query, true)?;
    Ok(upgrade
        .max_frame_size(MAX_FRAME_SIZE)
        .max_message_size(MAX_MESSAGE_SIZE)
        .on_upgrade(move |socket| bridge_socket(state, socket))
        .into_response())
}

async fn bridge_socket(state: AppState, socket: WebSocket) {
    let client_id = state.client_id();
    let (output_tx, output_rx) = RuntimeSession::client_mailbox();
    if send_runtime(
        &state.inner.commands,
        RuntimeCommand::AttachClient {
            client_id,
            output: output_tx,
        },
    )
    .await
    .is_err()
    {
        return;
    }

    let (mut sender, mut receiver) = socket.split();
    let mut output_rx = output_rx;
    loop {
        tokio::select! {
            Some(message) = receiver.next() => {
                match message {
                    Ok(Message::Binary(bytes)) => {
                        if send_runtime(&state.inner.commands, RuntimeCommand::Input { client_id, bytes }).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<ClientControlMessage>(&text) {
                            Ok(ClientControlMessage::Resize { cols, rows }) => {
                                let size = TerminalSize { cols, rows };
                                if send_runtime(&state.inner.commands, RuntimeCommand::Resize { size }).await.is_err() {
                                    break;
                                }
                            }
                            Ok(ClientControlMessage::Heartbeat { .. }) => {}
                            Err(error) => {
                                debug!(%error, "closing websocket after invalid control frame");
                                let _result = send_control_error(&mut sender).await;
                                let _result = sender.send(Message::Close(Some(protocol_close()))).await;
                                break;
                            }
                        }
                    }
                    Ok(Message::Close(_frame)) => break,
                    Ok(Message::Ping(bytes)) => {
                        if sender.send(Message::Pong(bytes)).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Pong(_bytes)) => {}
                    Err(error) => {
                        debug!(%error, "websocket receive error");
                        break;
                    }
                }
            }
            output = output_rx.recv() => {
                if let Some(output) = output {
                    if send_client_output(&mut sender, output).await.is_err() {
                        break;
                    }
                } else {
                    let _result = sender.send(Message::Close(Some(backpressure_close()))).await;
                    break;
                }
            }
            else => break,
        }
    }

    let _result = send_runtime(
        &state.inner.commands,
        RuntimeCommand::DetachClient { client_id },
    )
    .await;
}

async fn send_client_output(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    output: ClientOutput,
) -> Result<(), axum::Error> {
    match output {
        ClientOutput::Bytes(bytes) => sender.send(Message::Binary(bytes)).await,
        ClientOutput::Control(control) => {
            let text = match serde_json::to_string(&control) {
                Ok(text) => text,
                Err(error) => {
                    debug!(%error, "failed to serialize control frame");
                    return Ok(());
                }
            };
            sender.send(Message::Text(text.into())).await
        }
        ClientOutput::Closed(_reason) => sender.send(Message::Close(None)).await,
    }
}

async fn send_control_error(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
) -> Result<(), axum::Error> {
    let message = ServerControlMessage::Error {
        code: ErrorCode::Protocol,
        message: protocol_error_message(),
    };
    let text = match serde_json::to_string(&message) {
        Ok(text) => text,
        Err(error) => {
            debug!(%error, "failed to serialize protocol error");
            return Ok(());
        }
    };
    sender.send(Message::Text(text.into())).await
}

fn protocol_error_message() -> SafeMessage {
    match SafeMessage::new("invalid control frame") {
        Ok(message) => message,
        Err(_error) => SafeMessage::from_static("protocol error"),
    }
}

fn protocol_close() -> CloseFrame {
    CloseFrame {
        code: close_code::PROTOCOL,
        reason: "invalid control frame".into(),
    }
}

fn backpressure_close() -> CloseFrame {
    CloseFrame {
        code: close_code::POLICY,
        reason: "browser client backpressure".into(),
    }
}

async fn send_runtime(
    commands: &mpsc::Sender<RuntimeCommand>,
    command: RuntimeCommand,
) -> Result<(), ()> {
    commands.send(command).await.map_err(|_error| ())
}

fn validate_http_request(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
    query: &TokenQuery,
    require_origin: bool,
) -> Result<(), WebError> {
    validate_peer_for_policy(peer, &state.inner.exposure)?;
    validate_host(headers, &state.inner.exposure)?;
    validate_token(&state.inner.token, &query.token)?;
    validate_origin(headers, &state.inner.exposure, require_origin)?;
    Ok(())
}

fn validate_asset_request(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
) -> Result<(), WebError> {
    validate_peer_for_policy(peer, &state.inner.exposure)?;
    validate_host(headers, &state.inner.exposure)?;
    Ok(())
}

fn validate_host(headers: &HeaderMap, exposure: &ExposurePolicy) -> Result<(), SecurityError> {
    let value = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or(SecurityError::InvalidHost)?;
    AllowedHost::validate_for_policy(value, exposure).map(|_host| ())
}

fn validate_token(expected: &AccessToken, supplied: &str) -> Result<(), SecurityError> {
    let supplied = AccessToken::from_str(supplied).map_err(|_error| SecurityError::InvalidToken)?;
    validate_access_token(expected, &supplied)
}

fn validate_origin(
    headers: &HeaderMap,
    exposure: &ExposurePolicy,
    required: bool,
) -> Result<(), SecurityError> {
    let Some(value) = headers.get(header::ORIGIN) else {
        return if required {
            Err(SecurityError::InvalidOrigin)
        } else {
            Ok(())
        };
    };
    let value = value
        .to_str()
        .map_err(|_error| SecurityError::InvalidOrigin)?;
    AllowedOrigin::validate_for_policy(value, exposure).map(|_origin| ())
}

#[derive(Debug)]
struct WebError(SecurityError);

impl From<SecurityError> for WebError {
    fn from(value: SecurityError) -> Self {
        Self(value)
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        debug!(reason = ?self.0, "forbidden browser terminal request");
        StatusCode::FORBIDDEN.into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use anyhow::Context;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use termstage_core::{
        protocol::{AccessToken, SessionName, TerminalSize},
        runtime::{ReconnectPolicy, RuntimeSession, SessionMode, ShellCommand, ShutdownReason},
    };
    use tokio::time::{Duration, timeout};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Message as TungsteniteMessage, client::IntoClientRequest},
    };
    use tower::ServiceExt;

    use super::*;

    fn test_shell_command() -> anyhow::Result<ShellCommand> {
        ShellCommand::new(
            "/bin/bash",
            [
                std::ffi::OsString::from("--noprofile"),
                std::ffi::OsString::from("--norc"),
            ],
        )
        .map_err(Into::into)
    }

    fn test_config() -> anyhow::Result<(WebConfig, AccessToken)> {
        let token = AccessToken::from_bytes([9; 32]);
        let (commands, _rx) = mpsc::channel(8);
        let shell = test_shell_command()?;
        let runtime = RuntimeConfig {
            mode: SessionMode::NewShell { shell },
            initial_size: TerminalSize::new(80, 24)?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
        };
        let mut config = WebConfig::local(token.clone(), commands, runtime);
        config.port = 49152;
        Ok((config, token))
    }

    async fn request(path: &str, host: &str, config: WebConfig) -> anyhow::Result<StatusCode> {
        let app = router(config)?;
        let response = app
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::HOST, host)
                    .extension(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 50000))))
                    .body(Body::empty())?,
            )
            .await?;
        Ok(response.status())
    }

    async fn request_from_peer(
        path: &str,
        host: &str,
        peer: SocketAddr,
        config: WebConfig,
    ) -> anyhow::Result<StatusCode> {
        let app = router(config)?;
        let response = app
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::HOST, host)
                    .extension(ConnectInfo(peer))
                    .body(Body::empty())?,
            )
            .await?;
        Ok(response.status())
    }

    #[tokio::test]
    async fn test_should_serve_index_with_valid_token_and_host() -> anyhow::Result<()> {
        let (config, token) = test_config()?;
        let path = format!("/?token={}", token.to_url_token());
        let app = router(config)?;
        let response = app
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::HOST, "127.0.0.1:49152")
                    .extension(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 50000))))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await?.to_bytes();
        assert!(!body.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_should_reject_invalid_token_and_host() -> anyhow::Result<()> {
        let (config, token) = test_config()?;
        let good_path = format!("/?token={}", token.to_url_token());
        assert_eq!(
            request(&good_path, "example.com:49152", config.clone()).await?,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request("/?token=bad", "127.0.0.1:49152", config).await?,
            StatusCode::FORBIDDEN
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_should_reject_non_loopback_peer() -> anyhow::Result<()> {
        let (config, token) = test_config()?;
        let app = router(config)?;
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/?token={}", token.to_url_token()))
                    .header(header::HOST, "127.0.0.1:49152")
                    .extension(ConnectInfo(SocketAddr::from(([192, 0, 2, 10], 50000))))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn test_should_accept_public_host_and_non_loopback_peer() -> anyhow::Result<()> {
        let (mut config, token) = test_config()?;
        config.host = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        config.exposure = WebExposure::Public {
            public_url: PublicBaseUrl::parse("https://term.example.com/")?,
        };
        let path = format!("/?token={}", token.to_url_token());
        assert_eq!(
            request_from_peer(
                &path,
                "term.example.com",
                SocketAddr::from(([192, 0, 2, 10], 50000)),
                config.clone(),
            )
            .await?,
            StatusCode::OK
        );
        assert_eq!(
            request_from_peer(
                &path,
                "evil.example",
                SocketAddr::from(([192, 0, 2, 10], 50000)),
                config,
            )
            .await?,
            StatusCode::FORBIDDEN
        );
        Ok(())
    }

    #[test]
    fn test_should_validate_public_websocket_origin() -> anyhow::Result<()> {
        let (mut config, token) = test_config()?;
        config.host = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        config.exposure = WebExposure::Public {
            public_url: PublicBaseUrl::parse("https://term.example.com/")?,
        };
        let state = AppState::new(config)?;
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "term.example.com".parse()?);
        headers.insert(header::ORIGIN, "https://term.example.com".parse()?);
        let query = TokenQuery {
            token: token.to_url_token(),
        };
        assert!(
            validate_http_request(
                &state,
                SocketAddr::from(([192, 0, 2, 10], 50000)),
                &headers,
                &query,
                true,
            )
            .is_ok()
        );
        headers.insert(header::ORIGIN, "https://evil.example".parse()?);
        assert!(matches!(
            validate_http_request(
                &state,
                SocketAddr::from(([192, 0, 2, 10], 50000)),
                &headers,
                &query,
                true,
            ),
            Err(WebError(SecurityError::InvalidOrigin))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_should_build_public_launch_url() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([4; 32]);
        let server = RunningServer {
            address: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 8080)),
            token: token.clone(),
            presentation: PresentationSettings::default(),
            exposure: WebExposure::Public {
                public_url: PublicBaseUrl::parse("https://term.example.com/")?,
            },
            base_path: None,
            shutdown: None,
            task: tokio::spawn(async { Ok(()) }),
        };
        assert_eq!(
            server.launch_url(),
            format!(
                "https://term.example.com/?token={}&fontSize=24&theme=high-contrast",
                token.to_url_token()
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_should_validate_asset_host_and_peer() -> anyhow::Result<()> {
        let (config, _token) = test_config()?;
        assert_eq!(
            request("/assets/index.js", "127.0.0.1:49152", config.clone()).await?,
            StatusCode::OK
        );
        assert_eq!(
            request("/assets/index.js", "example.com:49152", config).await?,
            StatusCode::FORBIDDEN
        );
        Ok(())
    }

    #[test]
    fn test_should_reject_invalid_websocket_origin() -> anyhow::Result<()> {
        let (config, token) = test_config()?;
        let state = AppState::new(config)?;
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:49152".parse()?);
        headers.insert(header::ORIGIN, "http://evil.example".parse()?);
        let query = TokenQuery {
            token: token.to_url_token(),
        };
        assert!(matches!(
            validate_http_request(
                &state,
                SocketAddr::from((Ipv4Addr::LOCALHOST, 50000)),
                &headers,
                &query,
                true,
            ),
            Err(WebError(SecurityError::InvalidOrigin))
        ));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_bridge_websocket_binary_input_to_runtime_output() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([5; 32]);
        let runtime = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: TerminalSize::new(80, 24)?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
        };
        let session = RuntimeSession::start(runtime.clone())?;
        let mut config = WebConfig::local(token.clone(), session.command_sender(), runtime);
        config.port = 0;
        let server = serve(config).await?;
        let url = format!(
            "ws://{}/ws?token={}",
            server.address(),
            token.to_url_token()
        );
        let mut request = url.into_client_request()?;
        let origin = format!("http://{}", server.address());
        request
            .headers_mut()
            .insert(header::ORIGIN, origin.parse()?);
        let (mut socket, _response) = connect_async(request).await?;
        socket
            .send(TungsteniteMessage::Binary(Bytes::from_static(
                b"printf phase3-ws-ok\\n\n",
            )))
            .await?;
        let mut aggregate = Vec::new();
        let found = timeout(Duration::from_secs(5), async {
            while let Some(message) = socket.next().await {
                match message? {
                    TungsteniteMessage::Binary(bytes) => {
                        aggregate.extend_from_slice(&bytes);
                        if aggregate
                            .windows(b"phase3-ws-ok".len())
                            .any(|window| window == b"phase3-ws-ok")
                        {
                            return anyhow::Ok(true);
                        }
                    }
                    TungsteniteMessage::Text(_text) => {}
                    TungsteniteMessage::Close(_frame) => return anyhow::Ok(false),
                    TungsteniteMessage::Ping(_bytes) | TungsteniteMessage::Pong(_bytes) => {}
                    TungsteniteMessage::Frame(_frame) => {}
                }
            }
            anyhow::Ok(false)
        })
        .await??;
        assert!(found);
        server.shutdown().await?;
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_reattach_websocket_after_browser_refresh() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([7; 32]);
        let runtime = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: TerminalSize::new(80, 24)?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
        };
        let session = RuntimeSession::start(runtime.clone())?;
        let config = WebConfig::local(token.clone(), session.command_sender(), runtime);
        let server = serve(config).await?;

        let mut first = connect_test_socket(server.address(), &token).await?;
        first
            .send(TungsteniteMessage::Binary(Bytes::from_static(
                b"printf phase5-before-refresh\\n\n",
            )))
            .await?;
        assert!(
            read_socket_until(&mut first, b"phase5-before-refresh").await?,
            "first websocket did not receive terminal output"
        );
        first.close(None).await?;

        let mut second = connect_test_socket(server.address(), &token).await?;
        assert!(
            read_socket_until(&mut second, b"phase5-before-refresh").await?,
            "second websocket did not receive replayed terminal state"
        );
        second
            .send(TungsteniteMessage::Binary(Bytes::from_static(
                b"printf phase5-after-refresh\\n\n",
            )))
            .await?;
        assert!(
            read_socket_until(&mut second, b"phase5-after-refresh").await?,
            "second websocket did not receive terminal output"
        );
        server.shutdown().await?;
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_replay_tmux_state_after_browser_refresh() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([8; 32]);
        let session_name = SessionName::new(format!("termstage-phase5-{}", std::process::id()))?;
        let runtime = RuntimeConfig {
            mode: SessionMode::Tmux {
                session: session_name.clone(),
            },
            initial_size: TerminalSize::new(80, 24)?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
        };
        let session = RuntimeSession::start(runtime.clone())?;
        let config = WebConfig::local(token.clone(), session.command_sender(), runtime);
        let server = serve(config).await?;

        let mut first = connect_test_socket(server.address(), &token).await?;
        first
            .send(TungsteniteMessage::Binary(Bytes::from_static(
                b"printf phase5-tmux-state\\n\n",
            )))
            .await?;
        assert!(
            read_socket_until(&mut first, b"phase5-tmux-state").await?,
            "first websocket did not receive tmux output"
        );
        first.close(None).await?;

        let mut second = connect_test_socket(server.address(), &token).await?;
        assert!(
            read_socket_until(&mut second, b"phase5-tmux-state").await?,
            "second websocket did not receive replayed tmux state"
        );
        second
            .send(TungsteniteMessage::Binary(Bytes::from_static(b"exit\n")))
            .await?;
        server.shutdown().await?;
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_close_websocket_when_runtime_drops_client_mailbox() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([10; 32]);
        let (commands, mut command_rx) = mpsc::channel(8);
        let runtime = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: TerminalSize::new(80, 24)?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
        };
        let server = serve(WebConfig::local(token.clone(), commands, runtime)).await?;
        let runtime_task = tokio::spawn(async move {
            if let Some(RuntimeCommand::AttachClient { output, .. }) = command_rx.recv().await {
                drop(output);
            }
        });

        let mut socket = connect_test_socket(server.address(), &token).await?;
        let close = timeout(Duration::from_secs(5), socket.next()).await?;
        assert!(matches!(close, Some(Ok(TungsteniteMessage::Close(_frame)))));
        runtime_task.await.context("fake runtime task panicked")?;
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_close_oversized_websocket_frame() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([6; 32]);
        let runtime = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: TerminalSize::new(80, 24)?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
        };
        let session = RuntimeSession::start(runtime.clone())?;
        let config = WebConfig::local(token.clone(), session.command_sender(), runtime);
        let server = serve(config).await?;
        let url = format!(
            "ws://{}/ws?token={}",
            server.address(),
            token.to_url_token()
        );
        let mut request = url.into_client_request()?;
        let origin = format!("http://{}", server.address());
        request
            .headers_mut()
            .insert(header::ORIGIN, origin.parse()?);
        let (mut socket, _response) = connect_async(request).await?;
        socket
            .send(TungsteniteMessage::Binary(Bytes::from(vec![
                b'x';
                MAX_MESSAGE_SIZE
                    + 1
            ])))
            .await?;
        let closed = timeout(Duration::from_secs(5), socket.next()).await?;
        assert!(closed.is_some());
        server.shutdown().await?;
        session.shutdown(ShutdownReason::Supervisor).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_should_mount_routes_under_base_path() -> anyhow::Result<()> {
        let (mut config, token) = test_config()?;
        config.base_path = Some(BasePath::parse("/p/sess-1/")?);
        let token_value = token.to_url_token();
        let app = router(config.clone())?;
        let prefixed = format!("/p/sess-1/?token={token_value}");
        let prefixed_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&prefixed)
                    .header(header::HOST, "127.0.0.1:49152")
                    .extension(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 50000))))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(prefixed_response.status(), StatusCode::OK);
        let body_bytes = prefixed_response.into_body().collect().await?.to_bytes();
        let body = std::str::from_utf8(&body_bytes)?;
        assert!(
            body.contains("<base href=\"/p/sess-1/\">"),
            "expected base href tag, got: {body}"
        );

        let root_response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/?token={token_value}"))
                    .header(header::HOST, "127.0.0.1:49152")
                    .extension(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 50000))))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(root_response.status(), StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn test_should_render_default_index_without_base_href() -> anyhow::Result<()> {
        let (config, token) = test_config()?;
        let app = router(config)?;
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/?token={}", token.to_url_token()))
                    .header(header::HOST, "127.0.0.1:49152")
                    .extension(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 50000))))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = response.into_body().collect().await?.to_bytes();
        let body = std::str::from_utf8(&body_bytes)?;
        assert!(!body.contains("<base href"));
        Ok(())
    }

    #[tokio::test]
    async fn test_should_build_local_launch_url_with_base_path() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([4; 32]);
        let server = RunningServer {
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, 8080)),
            token: token.clone(),
            presentation: PresentationSettings::default(),
            exposure: WebExposure::Local,
            base_path: Some(BasePath::parse("/p/sess-2/")?),
            shutdown: None,
            task: tokio::spawn(async { Ok(()) }),
        };
        let url = server.launch_url();
        assert!(url.starts_with("http://127.0.0.1:8080/p/sess-2/?"), "{url}");
        assert!(url.contains("token="));
        Ok(())
    }

    #[tokio::test]
    async fn test_should_reject_non_loopback_bind() -> anyhow::Result<()> {
        let (mut config, _token) = test_config()?;
        config.host = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        assert!(matches!(
            router(config).context("router unexpectedly succeeded"),
            Err(_error)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_should_build_launch_url_without_debug_token() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([4; 32]);
        let server = RunningServer::for_test(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 49152)),
            token,
            PresentationSettings::default(),
        );
        assert!(server.launch_url().contains("token="));
        assert!(!format!("{server:?}").contains("0404"));
        let _session = SessionName::new("presentation")?;
        Ok(())
    }

    async fn connect_test_socket(
        address: SocketAddr,
        token: &AccessToken,
    ) -> anyhow::Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    > {
        let url = format!("ws://{address}/ws?token={}", token.to_url_token());
        let mut request = url.into_client_request()?;
        let origin = format!("http://{address}");
        request
            .headers_mut()
            .insert(header::ORIGIN, origin.parse()?);
        let (socket, _response) = connect_async(request).await?;
        Ok(socket)
    }

    async fn read_socket_until(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        needle: &[u8],
    ) -> anyhow::Result<bool> {
        let mut aggregate = Vec::new();
        timeout(Duration::from_secs(5), async {
            while let Some(message) = socket.next().await {
                match message? {
                    TungsteniteMessage::Binary(bytes) => {
                        aggregate.extend_from_slice(&bytes);
                        if aggregate
                            .windows(needle.len())
                            .any(|window| window == needle)
                        {
                            return anyhow::Ok(true);
                        }
                    }
                    TungsteniteMessage::Text(_text) => {}
                    TungsteniteMessage::Close(_frame) => return anyhow::Ok(false),
                    TungsteniteMessage::Ping(_bytes) | TungsteniteMessage::Pong(_bytes) => {}
                    TungsteniteMessage::Frame(_frame) => {}
                }
            }
            anyhow::Ok(false)
        })
        .await?
    }
}
