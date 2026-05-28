//! Axum routes and WebSocket bridge for browser terminal mode.

use std::{
    fmt::{self, Debug, Formatter, Write as FmtWrite},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{
        ConnectInfo, DefaultBodyLimit, Path, Query, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use termstage_core::{
    backend::{BackendScreenSnapshot, BackendScrollDirection},
    operation_lock::{ControllerId, ControllerKind, ControllerRef, OperationLockError},
    protocol::{
        AccessToken, ClientControlMessage, ErrorCode, LeaseOwner, SafeMessage,
        ServerControlMessage, SessionName, TerminalSize, ViewportOrigin,
    },
    runtime::{ClientId, RuntimeCommand, RuntimeConfig},
    security::{
        AllowedHost, AllowedOrigin, BasePath, ExposurePolicy, PublicBaseUrl, SecurityError,
        validate_access_token, validate_peer_for_policy,
    },
    session_gateway::{SessionGateway, SessionGatewayError},
    tmux_backend::TmuxBackend,
    tunnel::{
        RuntimeTunnelBridge, RuntimeTunnelBridgeOutcome, TunnelCloseReason, TunnelError,
        TunnelFrame, TunnelRuntimeControl, TunnelTerminalPayload, TunnelTransport,
    },
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
    time,
};
use tracing::debug;

use crate::assets::{asset_response, index_response};

type BrowserSocketSender = futures_util::stream::SplitSink<WebSocket, Message>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GatewayBrowserLease {
    owns_lease: bool,
    owner: LeaseOwner,
    epoch: u64,
    native_attached: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GatewayViewport {
    size: TerminalSize,
    origin_col: Option<u16>,
    origin_row: Option<u16>,
}

impl GatewayViewport {
    const fn new(size: TerminalSize) -> Self {
        Self {
            size,
            origin_col: None,
            origin_row: None,
        }
    }

    const fn size(self) -> TerminalSize {
        self.size
    }

    fn update_size(&mut self, size: TerminalSize) {
        self.size = size;
    }

    fn update_origin(&mut self, col: Option<u16>, row: Option<u16>) {
        if let Some(col) = col {
            self.origin_col = Some(col);
        }
        if let Some(row) = row {
            self.origin_row = Some(row);
        }
    }

    const fn origin_col(self) -> Option<u16> {
        self.origin_col
    }

    const fn origin_row(self) -> Option<u16> {
        self.origin_row
    }
}

const DEFAULT_BIND_HOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const MAX_FRAME_SIZE: usize = 16 * 1024;
const MAX_MESSAGE_SIZE: usize = 64 * 1024;
const API_BODY_LIMIT_BYTES: usize = 8 * 1024;
const SEMANTIC_TEXT_MAX_BYTES: usize = 4096;
const SEMANTIC_KEY_MAX_BYTES: usize = 32;
const SEMANTIC_SCROLL_MAX_AMOUNT: u16 = 100;
const SEMANTIC_WAIT_TEXT_MAX_BYTES: usize = 4096;
const SEMANTIC_WAIT_TIMEOUT_DEFAULT_MS: u64 = 5_000;
const SEMANTIC_WAIT_TIMEOUT_MAX_MS: u64 = 60_000;
const SEMANTIC_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const CLIENT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);
const CLIENT_HEARTBEAT_CHECK_INTERVAL: Duration = Duration::from_secs(10);
const SERVER_PING_INTERVAL: Duration = Duration::from_secs(25);
const GATEWAY_SCREEN_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SERVER_PING_PAYLOAD: &[u8] = b"termstage";
const TUNNEL_CHANNEL_CAPACITY: usize = 32;
const CLOSE_REASON_SESSION_ENDED: &str = "session ended";
const CLOSE_REASON_SERVER_SHUTDOWN: &str = "server shutting down";
const CLOSE_REASON_CLIENT_DISCONNECTED: &str = "client disconnected";
const CLOSE_REASON_CONTROLLER_REPLACED: &str = "controller replaced";
const CLOSE_REASON_RUNTIME_ERROR: &str = "runtime error";

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
    /// Browser terminal session backend.
    pub session: WebSession,
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
        _runtime: RuntimeConfig,
    ) -> Self {
        Self {
            host: DEFAULT_BIND_HOST,
            port: 0,
            exposure: WebExposure::Local,
            token,
            session: WebSession::Runtime { commands },
            presentation: PresentationSettings::default(),
            base_path: None,
        }
    }

    /// Creates a local config backed by a tmux session gateway.
    #[must_use]
    pub fn local_tmux_gateway(
        token: AccessToken,
        gateway: Arc<Mutex<SessionGateway<TmuxBackend>>>,
        session: SessionName,
    ) -> Self {
        Self {
            host: DEFAULT_BIND_HOST,
            port: 0,
            exposure: WebExposure::Local,
            token,
            session: WebSession::TmuxGateway { gateway, session },
            presentation: PresentationSettings::default(),
            base_path: None,
        }
    }
}

/// Browser terminal session backend.
#[derive(Debug, Clone)]
pub enum WebSession {
    /// Existing PTY runtime actor path used by shell mode.
    Runtime {
        /// Runtime command sender.
        commands: mpsc::Sender<RuntimeCommand>,
    },
    /// Backend-owned tmux session gateway path.
    TmuxGateway {
        /// Shared session gateway.
        gateway: Arc<Mutex<SessionGateway<TmuxBackend>>>,
        /// Termstage session id to expose in this web server.
        session: SessionName,
    },
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
    session: WebSession,
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
            .field("session", &self.inner.session)
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
                session: config.session,
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
        .route(
            &format!("{prefix}/api/sessions/{{session}}/acquire-lock"),
            post(api_acquire_lock),
        )
        .route(
            &format!("{prefix}/api/sessions/{{session}}/release-lock"),
            post(api_release_lock),
        )
        .route(
            &format!("{prefix}/api/sessions/{{session}}/press-key"),
            post(api_press_key),
        )
        .route(
            &format!("{prefix}/api/sessions/{{session}}/write-text"),
            post(api_write_text),
        )
        .route(
            &format!("{prefix}/api/sessions/{{session}}/run-command"),
            post(api_run_command),
        )
        .route(
            &format!("{prefix}/api/sessions/{{session}}/read-screen"),
            post(api_read_screen),
        )
        .route(
            &format!("{prefix}/api/sessions/{{session}}/scroll"),
            post(api_scroll),
        )
        .route(&format!("{prefix}/healthz"), get(healthz));
    if !prefix.is_empty() {
        router = router.route(prefix, get(index));
    }
    Ok(router
        .layer(DefaultBodyLimit::max(API_BODY_LIMIT_BYTES))
        .with_state(state))
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
    #[serde(default)]
    cols: Option<u16>,
    #[serde(default)]
    rows: Option<u16>,
}

impl TokenQuery {
    fn browser_viewport(&self) -> Option<GatewayViewport> {
        let (Some(cols), Some(rows)) = (self.cols, self.rows) else {
            return None;
        };
        TerminalSize::new(cols, rows).ok().map(GatewayViewport::new)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiSessionPath {
    session: SessionName,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControllerRequest {
    controller_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PressKeyRequest {
    controller_id: u64,
    key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WriteTextRequest {
    controller_id: u64,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RunCommandRequest {
    controller_id: u64,
    command: String,
    #[serde(default)]
    wait_for: Option<String>,
    #[serde(default)]
    wait_timeout_ms: Option<u64>,
    #[serde(default)]
    capture: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScrollRequest {
    controller_id: u64,
    direction: ScrollDirection,
    amount: u16,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ScrollDirection {
    Up,
    Down,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LeaseResponse {
    owner: LeaseOwner,
    epoch: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OperationResponse {
    ok: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunCommandResponse {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    matched: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    screen: Option<ScreenResponse>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScreenResponse {
    size: TerminalSize,
    cursor_col: u16,
    cursor_row: u16,
    lines: Vec<String>,
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
    let browser_viewport = query.browser_viewport();
    Ok(upgrade
        .max_frame_size(MAX_FRAME_SIZE)
        .max_message_size(MAX_MESSAGE_SIZE)
        .on_upgrade(move |socket| bridge_socket(state, socket, browser_viewport))
        .into_response())
}

async fn api_acquire_lock(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(path): Path<ApiSessionPath>,
    Query(query): Query<TokenQuery>,
    Json(request): Json<ControllerRequest>,
) -> Result<Json<LeaseResponse>, ApiError> {
    validate_api_request(&state, peer, &headers, &query)?;
    let controller = agent_controller(request.controller_id)?;
    let gateway = gateway_for_session(&state, &path.session)?;
    let mut gateway = gateway.lock().await;
    let lease = gateway
        .acquire_controller(&path.session, controller, Instant::now())
        .map_err(ApiError::from_gateway)?;
    Ok(Json(LeaseResponse {
        owner: LeaseOwner::Agent,
        epoch: lease.epoch(),
    }))
}

async fn api_release_lock(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(path): Path<ApiSessionPath>,
    Query(query): Query<TokenQuery>,
    Json(request): Json<ControllerRequest>,
) -> Result<Json<LeaseResponse>, ApiError> {
    validate_api_request(&state, peer, &headers, &query)?;
    let controller = agent_controller(request.controller_id)?;
    let gateway = gateway_for_session(&state, &path.session)?;
    let mut gateway = gateway.lock().await;
    let lease = gateway
        .release_controller(&path.session, controller, Instant::now())
        .map_err(ApiError::from_gateway)?;
    Ok(Json(LeaseResponse {
        owner: LeaseOwner::Terminal,
        epoch: lease.epoch(),
    }))
}

async fn api_press_key(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(path): Path<ApiSessionPath>,
    Query(query): Query<TokenQuery>,
    Json(request): Json<PressKeyRequest>,
) -> Result<Json<OperationResponse>, ApiError> {
    validate_api_request(&state, peer, &headers, &query)?;
    let key = semantic_key_token(&request.key)?;
    send_semantic_key(&state, &path.session, request.controller_id, &key).await?;
    Ok(Json(OperationResponse { ok: true }))
}

async fn api_write_text(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(path): Path<ApiSessionPath>,
    Query(query): Query<TokenQuery>,
    Json(request): Json<WriteTextRequest>,
) -> Result<Json<OperationResponse>, ApiError> {
    validate_api_request(&state, peer, &headers, &query)?;
    let text = bounded_semantic_text(request.text)?;
    send_semantic_text(&state, &path.session, request.controller_id, &text).await?;
    Ok(Json(OperationResponse { ok: true }))
}

async fn api_run_command(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(path): Path<ApiSessionPath>,
    Query(query): Query<TokenQuery>,
    Json(request): Json<RunCommandRequest>,
) -> Result<Json<RunCommandResponse>, ApiError> {
    validate_api_request(&state, peer, &headers, &query)?;
    let command = semantic_command_text(request.command)?;
    let wait_for = request.wait_for.map(bounded_wait_text).transpose()?;
    let wait_timeout = semantic_wait_timeout(request.wait_timeout_ms)?;
    let controller = agent_controller(request.controller_id)?;
    let gateway = gateway_for_session(&state, &path.session)?;
    {
        let mut gateway = gateway.lock().await;
        gateway
            .run_command(&path.session, controller, &command, Instant::now())
            .await
            .map_err(ApiError::from_gateway)?;
    }

    let mut matched = None;
    let mut screen = None;
    if let Some(wait_for) = wait_for.as_deref() {
        let wait_result =
            wait_for_screen_text(&gateway, &path.session, wait_for, wait_timeout).await?;
        matched = Some(wait_result.matched);
        if request.capture {
            screen = wait_result.snapshot.as_ref().map(screen_response);
        }
    } else if request.capture {
        screen = Some(read_gateway_screen(&gateway, &path.session).await?);
    }

    Ok(Json(RunCommandResponse {
        ok: true,
        matched,
        screen,
    }))
}

async fn api_read_screen(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(path): Path<ApiSessionPath>,
    Query(query): Query<TokenQuery>,
) -> Result<Json<ScreenResponse>, ApiError> {
    validate_api_request(&state, peer, &headers, &query)?;
    let gateway = gateway_for_session(&state, &path.session)?;
    let snapshot = {
        let mut gateway = gateway.lock().await;
        gateway
            .read_screen(&path.session)
            .await
            .map_err(ApiError::from_gateway)?
    };
    Ok(Json(screen_response(&snapshot)))
}

async fn api_scroll(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(path): Path<ApiSessionPath>,
    Query(query): Query<TokenQuery>,
    Json(request): Json<ScrollRequest>,
) -> Result<Json<OperationResponse>, ApiError> {
    validate_api_request(&state, peer, &headers, &query)?;
    if request.amount == 0 || request.amount > SEMANTIC_SCROLL_MAX_AMOUNT {
        return Err(ApiError::BadRequest);
    }
    let controller = agent_controller(request.controller_id)?;
    let direction = match request.direction {
        ScrollDirection::Up => BackendScrollDirection::Up,
        ScrollDirection::Down => BackendScrollDirection::Down,
    };
    let gateway = gateway_for_session(&state, &path.session)?;
    let mut gateway = gateway.lock().await;
    gateway
        .scroll(
            &path.session,
            controller,
            direction,
            request.amount,
            Instant::now(),
        )
        .await
        .map_err(ApiError::from_gateway)?;
    Ok(Json(OperationResponse { ok: true }))
}

async fn bridge_socket(
    state: AppState,
    socket: WebSocket,
    browser_viewport: Option<GatewayViewport>,
) {
    match state.inner.session.clone() {
        WebSession::Runtime { commands } => bridge_runtime_socket(state, socket, commands).await,
        WebSession::TmuxGateway { gateway, session } => {
            bridge_gateway_socket(state, socket, gateway, session, browser_viewport).await;
        }
    }
}

async fn bridge_runtime_socket(
    state: AppState,
    socket: WebSocket,
    commands: mpsc::Sender<RuntimeCommand>,
) {
    let client_id = state.client_id();
    let (mut sender, mut receiver) = socket.split();
    let (mut tunnel, transport) = in_process_tunnel_pair();
    let mut bridge_task = tokio::spawn(RuntimeTunnelBridge::run(transport, commands));
    let mut bridge_finished = false;
    if tunnel
        .send_frame(TunnelFrame::AttachBrowser { client_id })
        .await
        .is_err()
    {
        let _result = sender
            .send(Message::Close(Some(runtime_unavailable_close())))
            .await;
        return;
    }

    let mut last_client_message = Instant::now();
    let mut heartbeat_check = time::interval(CLIENT_HEARTBEAT_CHECK_INTERVAL);
    let mut server_ping = time::interval(SERVER_PING_INTERVAL);
    loop {
        tokio::select! {
            result = &mut bridge_task, if !bridge_finished => {
                bridge_finished = true;
                handle_bridge_completion(result, &mut tunnel, &mut sender).await;
                break;
            }
            Some(message) = receiver.next() => {
                if handle_browser_message(
                    message,
                    client_id,
                    &mut tunnel,
                    &mut sender,
                    &mut last_client_message,
                ).await.should_close() {
                    break;
                }
            }
            frame = tunnel.receive_frame() => {
                if handle_tunnel_frame(frame, &mut sender).await.should_close() {
                    break;
                }
            }
            _ = heartbeat_check.tick() => {
                if last_client_message.elapsed() >= CLIENT_HEARTBEAT_TIMEOUT {
                    debug!(client_id = client_id.get(), "closing stale browser terminal websocket");
                    let _result = sender.send(Message::Close(Some(client_timeout_close()))).await;
                    break;
                }
            }
            _ = server_ping.tick() => {
                if send_server_ping(&mut sender).await.is_err() {
                    break;
                }
            }
            else => break,
        }
    }

    drop(tunnel);
    if !bridge_finished {
        match bridge_task.await {
            Ok(Ok(_outcome)) => {}
            Ok(Err(error)) => {
                debug!(%error, "runtime tunnel bridge closed during browser detach");
            }
            Err(error) => {
                debug!(%error, "runtime tunnel bridge task panicked during browser detach");
            }
        }
    }
}

async fn bridge_gateway_socket(
    state: AppState,
    socket: WebSocket,
    gateway: Arc<Mutex<SessionGateway<TmuxBackend>>>,
    session: SessionName,
    browser_viewport: Option<GatewayViewport>,
) {
    let client_id = state.client_id();
    let controller = match controller_from_client(client_id) {
        Ok(controller) => controller,
        Err(error) => {
            debug!(%error, "failed to create browser controller id");
            return;
        }
    };
    let (mut sender, mut receiver) = socket.split();
    let mut lease = match attach_gateway_browser(
        &gateway,
        &session,
        controller,
        browser_viewport,
        &mut sender,
    )
    .await
    {
        Ok(lease_state) => lease_state,
        Err(error) => {
            debug!(%error, "gateway browser attach failed");
            let _result = sender
                .send(Message::Close(Some(runtime_unavailable_close())))
                .await;
            return;
        }
    };

    let mut last_client_message = Instant::now();
    let mut browser_viewport = browser_viewport;
    let mut last_screen = Bytes::new();
    let mut heartbeat_check = time::interval(CLIENT_HEARTBEAT_CHECK_INTERVAL);
    let mut server_ping = time::interval(SERVER_PING_INTERVAL);
    let mut screen_poll = time::interval(GATEWAY_SCREEN_POLL_INTERVAL);
    loop {
        tokio::select! {
            Some(message) = receiver.next() => {
                last_client_message = Instant::now();
                if handle_gateway_browser_message(
                    message,
                    &gateway,
                    &session,
                    controller,
                    &mut lease,
                    &mut browser_viewport,
                    &mut sender,
                ).await.should_close() {
                    break;
                }
            }
            _ = screen_poll.tick() => {
                if sync_gateway_browser_lease(
                    &gateway,
                    &session,
                    controller,
                    &mut lease,
                    &mut sender,
                ).await.should_close() {
                    break;
                }
                if send_gateway_screen_if_changed(&gateway, &session, browser_viewport, &mut sender, &mut last_screen).await.should_close() {
                    break;
                }
            }
            _ = heartbeat_check.tick() => {
                if last_client_message.elapsed() >= CLIENT_HEARTBEAT_TIMEOUT {
                    debug!(client_id = client_id.get(), "closing stale gateway browser terminal websocket");
                    let _result = sender.send(Message::Close(Some(client_timeout_close()))).await;
                    break;
                }
            }
            _ = server_ping.tick() => {
                if send_server_ping(&mut sender).await.is_err() {
                    break;
                }
            }
            else => break,
        }
    }

    if lease.owns_lease {
        let mut gateway = gateway.lock().await;
        if let Err(error) = gateway.release_controller(&session, controller, Instant::now()) {
            debug!(%error, "failed to release gateway browser controller");
        }
    }
}

#[derive(Debug)]
enum BrowserSocketAction {
    Continue,
    Close,
}

impl BrowserSocketAction {
    const fn should_close(self) -> bool {
        matches!(self, Self::Close)
    }
}

async fn handle_bridge_completion(
    result: Result<Result<RuntimeTunnelBridgeOutcome, TunnelError>, tokio::task::JoinError>,
    tunnel: &mut BrowserTunnelPeer,
    sender: &mut BrowserSocketSender,
) {
    match result {
        Ok(Ok(_outcome)) => {
            let mut closed = false;
            while let Some(frame) = tunnel.try_receive_frame() {
                let result = send_tunnel_frame_to_browser(sender, frame).await;
                if result.should_close() || result.is_err() {
                    closed = true;
                    break;
                }
            }
            if !closed {
                let _result = sender
                    .send(Message::Close(Some(runtime_unavailable_close())))
                    .await;
            }
        }
        Ok(Err(error)) => {
            debug!(%error, "runtime tunnel bridge closed with error");
            let _result = sender
                .send(Message::Close(Some(runtime_unavailable_close())))
                .await;
        }
        Err(error) => {
            debug!(%error, "runtime tunnel bridge task panicked");
            let _result = sender
                .send(Message::Close(Some(runtime_unavailable_close())))
                .await;
        }
    }
}

async fn handle_browser_message(
    message: Result<Message, axum::Error>,
    client_id: ClientId,
    tunnel: &mut BrowserTunnelPeer,
    sender: &mut BrowserSocketSender,
    last_client_message: &mut Instant,
) -> BrowserSocketAction {
    match message {
        Ok(Message::Binary(bytes)) => {
            *last_client_message = Instant::now();
            handle_browser_binary(bytes, client_id, tunnel, sender).await
        }
        Ok(Message::Text(text)) => {
            *last_client_message = Instant::now();
            handle_browser_text(&text, client_id, tunnel, sender).await
        }
        Ok(Message::Close(_frame)) => BrowserSocketAction::Close,
        Ok(Message::Ping(bytes)) => {
            *last_client_message = Instant::now();
            if sender.send(Message::Pong(bytes)).await.is_err() {
                BrowserSocketAction::Close
            } else {
                BrowserSocketAction::Continue
            }
        }
        Ok(Message::Pong(_bytes)) => {
            *last_client_message = Instant::now();
            BrowserSocketAction::Continue
        }
        Err(error) => {
            debug!(%error, "websocket receive error");
            BrowserSocketAction::Close
        }
    }
}

async fn attach_gateway_browser(
    gateway: &Arc<Mutex<SessionGateway<TmuxBackend>>>,
    session: &SessionName,
    controller: ControllerRef,
    browser_viewport: Option<GatewayViewport>,
    sender: &mut BrowserSocketSender,
) -> Result<GatewayBrowserLease, SessionGatewayError> {
    let (owns_lease, lease_owner, lease_epoch, native_attached, snapshot) = {
        let mut gateway = gateway.lock().await;
        let native_attached = gateway.has_native_client(session).await?;
        let (owns_lease, lease_owner, lease_epoch) = if native_attached {
            (false, LeaseOwner::Terminal, 0)
        } else {
            match gateway.acquire_controller(session, controller, Instant::now()) {
                Ok(lease) => (true, LeaseOwner::Browser, lease.epoch()),
                Err(SessionGatewayError::Lock(OperationLockError::LeaseConflict {
                    owner, ..
                })) => (false, lease_owner_from_controller(owner), 0),
                Err(error) => return Err(error),
            }
        };
        let snapshot = gateway.read_screen(session).await?;
        (
            owns_lease,
            lease_owner,
            lease_epoch,
            native_attached,
            snapshot,
        )
    };
    send_server_control(
        sender,
        ServerControlMessage::LeaseChanged {
            owner: lease_owner,
            epoch: lease_epoch,
        },
    )
    .await;
    send_server_control(sender, ServerControlMessage::ReplayStarted).await;
    send_server_control(
        sender,
        ServerControlMessage::Ready {
            session: session.clone(),
        },
    )
    .await;
    let bytes = screen_snapshot_bytes(&snapshot, browser_viewport);
    let _result = sender.send(Message::Binary(bytes)).await;
    send_server_control(sender, ServerControlMessage::ReplayFinished).await;
    Ok(GatewayBrowserLease {
        owns_lease,
        owner: lease_owner,
        epoch: lease_epoch,
        native_attached,
    })
}

async fn sync_gateway_browser_lease(
    gateway: &Arc<Mutex<SessionGateway<TmuxBackend>>>,
    session: &SessionName,
    controller: ControllerRef,
    lease: &mut GatewayBrowserLease,
    sender: &mut BrowserSocketSender,
) -> BrowserSocketAction {
    let next = {
        let mut gateway = gateway.lock().await;
        match next_gateway_browser_lease(&mut gateway, session, controller, *lease).await {
            Ok(next) => next,
            Err(error) => {
                debug!(%error, "gateway lease sync failed");
                let _result = sender
                    .send(Message::Close(Some(runtime_unavailable_close())))
                    .await;
                return BrowserSocketAction::Close;
            }
        }
    };
    if next.owner != lease.owner || next.epoch != lease.epoch {
        send_server_control(
            sender,
            ServerControlMessage::LeaseChanged {
                owner: next.owner,
                epoch: next.epoch,
            },
        )
        .await;
    }
    *lease = next;
    BrowserSocketAction::Continue
}

async fn next_gateway_browser_lease(
    gateway: &mut SessionGateway<TmuxBackend>,
    session: &SessionName,
    controller: ControllerRef,
    current: GatewayBrowserLease,
) -> Result<GatewayBrowserLease, SessionGatewayError> {
    let native_attached = gateway.has_native_client(session).await?;
    if native_attached && !current.native_attached {
        if current.owns_lease {
            release_browser_controller(gateway, session, controller);
        }
        return Ok(GatewayBrowserLease {
            owns_lease: false,
            owner: LeaseOwner::Terminal,
            epoch: current.epoch,
            native_attached,
        });
    }
    if current.owns_lease {
        return Ok(GatewayBrowserLease {
            native_attached,
            ..current
        });
    }
    if native_attached {
        return Ok(GatewayBrowserLease {
            native_attached,
            ..current
        });
    }

    match gateway.acquire_controller(session, controller, Instant::now()) {
        Ok(lease) => Ok(GatewayBrowserLease {
            owns_lease: true,
            owner: LeaseOwner::Browser,
            epoch: lease.epoch(),
            native_attached,
        }),
        Err(SessionGatewayError::Lock(OperationLockError::LeaseConflict { owner, .. })) => {
            Ok(GatewayBrowserLease {
                owns_lease: false,
                owner: lease_owner_from_controller(owner),
                epoch: current.epoch,
                native_attached,
            })
        }
        Err(error) => Err(error),
    }
}

async fn acquire_gateway_browser_lease(
    gateway: &Arc<Mutex<SessionGateway<TmuxBackend>>>,
    session: &SessionName,
    controller: ControllerRef,
    lease: &mut GatewayBrowserLease,
    sender: &mut BrowserSocketSender,
) -> BrowserSocketAction {
    let next = {
        let mut gateway = gateway.lock().await;
        let native_attached = match gateway.has_native_client(session).await {
            Ok(attached) => attached,
            Err(error) => {
                debug!(%error, "gateway native-client state read failed during browser acquire");
                let _result = send_runtime_error(sender).await;
                return BrowserSocketAction::Continue;
            }
        };
        match gateway.acquire_controller(session, controller, Instant::now()) {
            Ok(operation_lease) => GatewayBrowserLease {
                owns_lease: true,
                owner: LeaseOwner::Browser,
                epoch: operation_lease.epoch(),
                native_attached,
            },
            Err(SessionGatewayError::Lock(OperationLockError::LeaseConflict { owner, .. })) => {
                GatewayBrowserLease {
                    owns_lease: false,
                    owner: lease_owner_from_controller(owner),
                    epoch: lease.epoch,
                    native_attached,
                }
            }
            Err(error) => {
                debug!(%error, "gateway browser acquire failed");
                let _result = send_runtime_error(sender).await;
                return BrowserSocketAction::Continue;
            }
        }
    };
    if next.owner != lease.owner || next.epoch != lease.epoch {
        send_server_control(
            sender,
            ServerControlMessage::LeaseChanged {
                owner: next.owner,
                epoch: next.epoch,
            },
        )
        .await;
    }
    *lease = next;
    BrowserSocketAction::Continue
}

fn release_browser_controller(
    gateway: &mut SessionGateway<TmuxBackend>,
    session: &SessionName,
    controller: ControllerRef,
) {
    if let Err(error) = gateway.release_controller(session, controller, Instant::now()) {
        debug!(%error, "failed to release browser controller during native tmux attach");
    }
}

const fn lease_owner_from_controller(controller: ControllerRef) -> LeaseOwner {
    match controller.kind() {
        ControllerKind::Browser => LeaseOwner::Browser,
        ControllerKind::Agent => LeaseOwner::Agent,
        ControllerKind::Transport => LeaseOwner::Terminal,
    }
}

async fn handle_gateway_browser_message(
    message: Result<Message, axum::Error>,
    gateway: &Arc<Mutex<SessionGateway<TmuxBackend>>>,
    session: &SessionName,
    controller: ControllerRef,
    lease: &mut GatewayBrowserLease,
    browser_viewport: &mut Option<GatewayViewport>,
    sender: &mut BrowserSocketSender,
) -> BrowserSocketAction {
    match message {
        Ok(Message::Binary(bytes)) => {
            handle_gateway_binary(bytes, gateway, session, controller, lease, sender).await
        }
        Ok(Message::Text(text)) => {
            handle_gateway_text(
                &text,
                gateway,
                session,
                controller,
                lease,
                browser_viewport,
                sender,
            )
            .await
        }
        Ok(Message::Close(_frame)) => BrowserSocketAction::Close,
        Ok(Message::Ping(bytes)) => {
            if sender.send(Message::Pong(bytes)).await.is_err() {
                BrowserSocketAction::Close
            } else {
                BrowserSocketAction::Continue
            }
        }
        Ok(Message::Pong(_bytes)) => BrowserSocketAction::Continue,
        Err(error) => {
            debug!(%error, "gateway websocket receive error");
            BrowserSocketAction::Close
        }
    }
}

async fn handle_gateway_binary(
    bytes: Bytes,
    gateway: &Arc<Mutex<SessionGateway<TmuxBackend>>>,
    session: &SessionName,
    controller: ControllerRef,
    lease: &GatewayBrowserLease,
    sender: &mut BrowserSocketSender,
) -> BrowserSocketAction {
    if !lease.owns_lease {
        let _result = send_forbidden_error(sender).await;
        return BrowserSocketAction::Continue;
    }
    let bytes = match TunnelTerminalPayload::new(bytes) {
        Ok(bytes) => bytes,
        Err(error) => {
            debug!(%error, "closing websocket after oversized gateway input frame");
            let _result = send_control_error(sender).await;
            let _result = sender.send(Message::Close(Some(protocol_close()))).await;
            return BrowserSocketAction::Close;
        }
    };
    let result = {
        let mut gateway = gateway.lock().await;
        gateway
            .write_input(session, controller, bytes.into_bytes(), Instant::now())
            .await
    };
    if let Err(error) = result {
        debug!(%error, "gateway input failed");
        let _result = send_runtime_error(sender).await;
    }
    BrowserSocketAction::Continue
}

async fn handle_gateway_text(
    text: &str,
    gateway: &Arc<Mutex<SessionGateway<TmuxBackend>>>,
    session: &SessionName,
    controller: ControllerRef,
    lease: &mut GatewayBrowserLease,
    browser_viewport: &mut Option<GatewayViewport>,
    sender: &mut BrowserSocketSender,
) -> BrowserSocketAction {
    let message = match serde_json::from_str::<ClientControlMessage>(text) {
        Ok(message) => message,
        Err(error) => {
            debug!(%error, "closing gateway websocket after invalid control frame");
            let _result = send_control_error(sender).await;
            let _result = sender.send(Message::Close(Some(protocol_close()))).await;
            return BrowserSocketAction::Close;
        }
    };
    match message {
        ClientControlMessage::AcquireControl => {
            return acquire_gateway_browser_lease(gateway, session, controller, lease, sender)
                .await;
        }
        ClientControlMessage::Resize { cols, rows } => {
            let size = TerminalSize { cols, rows };
            match browser_viewport {
                Some(viewport) => viewport.update_size(size),
                None => *browser_viewport = Some(GatewayViewport::new(size)),
            }
            debug!(
                ?size,
                owns_lease = lease.owns_lease,
                ?controller,
                "ignored browser resize for backend-owned gateway session"
            );
            send_server_control(sender, ServerControlMessage::SizeChanged { size }).await;
        }
        ClientControlMessage::Viewport { col, row } => {
            if let Some(viewport) = browser_viewport {
                viewport.update_origin(col.map(ViewportOrigin::get), row.map(ViewportOrigin::get));
                debug!(
                    ?viewport,
                    owns_lease = lease.owns_lease,
                    ?controller,
                    "updated browser viewport origin for backend-owned gateway session"
                );
            }
        }
        ClientControlMessage::Heartbeat { .. } => {}
    }
    BrowserSocketAction::Continue
}

async fn send_gateway_screen_if_changed(
    gateway: &Arc<Mutex<SessionGateway<TmuxBackend>>>,
    session: &SessionName,
    browser_viewport: Option<GatewayViewport>,
    sender: &mut BrowserSocketSender,
    last_screen: &mut Bytes,
) -> BrowserSocketAction {
    let snapshot = {
        let mut gateway = gateway.lock().await;
        match gateway.read_screen(session).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                debug!(%error, "gateway screen read failed");
                let _result = sender
                    .send(Message::Close(Some(runtime_unavailable_close())))
                    .await;
                return BrowserSocketAction::Close;
            }
        }
    };
    let bytes = screen_snapshot_bytes(&snapshot, browser_viewport);
    if bytes == *last_screen {
        return BrowserSocketAction::Continue;
    }
    *last_screen = bytes.clone();
    if sender.send(Message::Binary(bytes)).await.is_err() {
        BrowserSocketAction::Close
    } else {
        BrowserSocketAction::Continue
    }
}

async fn handle_browser_binary(
    bytes: Bytes,
    client_id: ClientId,
    tunnel: &BrowserTunnelPeer,
    sender: &mut BrowserSocketSender,
) -> BrowserSocketAction {
    let bytes = match TunnelTerminalPayload::new(bytes) {
        Ok(bytes) => bytes,
        Err(error) => {
            debug!(%error, "closing websocket after oversized tunnel input frame");
            let _result = send_control_error(sender).await;
            let _result = sender.send(Message::Close(Some(protocol_close()))).await;
            return BrowserSocketAction::Close;
        }
    };
    if tunnel
        .send_frame(TunnelFrame::BrowserInput { client_id, bytes })
        .await
        .is_err()
    {
        let _result = sender
            .send(Message::Close(Some(runtime_unavailable_close())))
            .await;
        BrowserSocketAction::Close
    } else {
        BrowserSocketAction::Continue
    }
}

async fn handle_browser_text(
    text: &str,
    client_id: ClientId,
    tunnel: &BrowserTunnelPeer,
    sender: &mut BrowserSocketSender,
) -> BrowserSocketAction {
    let frame = match serde_json::from_str::<ClientControlMessage>(text) {
        Ok(ClientControlMessage::AcquireControl) => {
            TunnelFrame::BrowserAcquireControl { client_id }
        }
        Ok(ClientControlMessage::Resize { cols, rows }) => TunnelFrame::BrowserResize {
            client_id,
            size: TerminalSize { cols, rows },
        },
        Ok(ClientControlMessage::Viewport { .. }) => return BrowserSocketAction::Continue,
        Ok(ClientControlMessage::Heartbeat { sequence }) => TunnelFrame::Heartbeat { sequence },
        Err(error) => {
            debug!(%error, "closing websocket after invalid control frame");
            let _result = send_control_error(sender).await;
            let _result = sender.send(Message::Close(Some(protocol_close()))).await;
            return BrowserSocketAction::Close;
        }
    };
    if tunnel.send_frame(frame).await.is_err() {
        let _result = sender
            .send(Message::Close(Some(runtime_unavailable_close())))
            .await;
        BrowserSocketAction::Close
    } else {
        BrowserSocketAction::Continue
    }
}

async fn handle_tunnel_frame(
    frame: Result<Option<TunnelFrame>, TunnelError>,
    sender: &mut BrowserSocketSender,
) -> BrowserSocketAction {
    match frame {
        Ok(Some(frame)) => {
            let result = send_tunnel_frame_to_browser(sender, frame).await;
            if result.should_close() || result.is_err() {
                BrowserSocketAction::Close
            } else {
                BrowserSocketAction::Continue
            }
        }
        Ok(None) => {
            let _result = sender
                .send(Message::Close(Some(runtime_unavailable_close())))
                .await;
            BrowserSocketAction::Close
        }
        Err(error) => {
            debug!(%error, "runtime tunnel receive failed");
            let _result = sender
                .send(Message::Close(Some(runtime_unavailable_close())))
                .await;
            BrowserSocketAction::Close
        }
    }
}

#[derive(Debug)]
struct BrowserTunnelPeer {
    inbound: mpsc::Receiver<TunnelFrame>,
    outbound: mpsc::Sender<TunnelFrame>,
}

impl BrowserTunnelPeer {
    async fn send_frame(&self, frame: TunnelFrame) -> Result<(), TunnelError> {
        self.outbound
            .send(frame)
            .await
            .map_err(|_error| TunnelError::TransportClosed)
    }

    async fn receive_frame(&mut self) -> Result<Option<TunnelFrame>, TunnelError> {
        Ok(self.inbound.recv().await)
    }

    fn try_receive_frame(&mut self) -> Option<TunnelFrame> {
        self.inbound.try_recv().ok()
    }
}

#[derive(Debug)]
struct InProcessTunnelTransport {
    inbound: mpsc::Receiver<TunnelFrame>,
    outbound: mpsc::Sender<TunnelFrame>,
}

impl TunnelTransport for InProcessTunnelTransport {
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

fn in_process_tunnel_pair() -> (BrowserTunnelPeer, InProcessTunnelTransport) {
    let (browser_to_runtime_tx, browser_to_runtime_rx) = mpsc::channel(TUNNEL_CHANNEL_CAPACITY);
    let (runtime_to_browser_tx, runtime_to_browser_rx) = mpsc::channel(TUNNEL_CHANNEL_CAPACITY);
    (
        BrowserTunnelPeer {
            inbound: runtime_to_browser_rx,
            outbound: browser_to_runtime_tx,
        },
        InProcessTunnelTransport {
            inbound: browser_to_runtime_rx,
            outbound: runtime_to_browser_tx,
        },
    )
}

#[derive(Debug)]
enum SendBrowserFrameResult {
    Continue(Result<(), axum::Error>),
    Closed(Result<(), axum::Error>),
}

impl SendBrowserFrameResult {
    fn is_err(&self) -> bool {
        match self {
            Self::Continue(result) | Self::Closed(result) => result.is_err(),
        }
    }

    fn should_close(&self) -> bool {
        matches!(self, Self::Closed(_result))
    }
}

async fn send_tunnel_frame_to_browser(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    frame: TunnelFrame,
) -> SendBrowserFrameResult {
    match frame {
        TunnelFrame::PtyOutput { bytes } => {
            SendBrowserFrameResult::Continue(sender.send(Message::Binary(bytes.into_bytes())).await)
        }
        TunnelFrame::RuntimeControl { control, .. } => match control {
            TunnelRuntimeControl::Server { message } => {
                let text = match serde_json::to_string(&message) {
                    Ok(text) => text,
                    Err(error) => {
                        debug!(%error, "failed to serialize control frame");
                        return SendBrowserFrameResult::Continue(Ok(()));
                    }
                };
                SendBrowserFrameResult::Continue(sender.send(Message::Text(text.into())).await)
            }
            TunnelRuntimeControl::Closed { reason } => {
                if reason == TunnelCloseReason::ChildExit {
                    let _result = send_process_exited(sender).await;
                }
                SendBrowserFrameResult::Closed(
                    sender
                        .send(Message::Close(Some(close_frame_for_tunnel_reason(reason))))
                        .await,
                )
            }
        },
        TunnelFrame::Heartbeat { .. } => SendBrowserFrameResult::Continue(Ok(())),
        TunnelFrame::RegisterSession { .. }
        | TunnelFrame::AttachBrowser { .. }
        | TunnelFrame::DetachBrowser { .. }
        | TunnelFrame::BrowserInput { .. }
        | TunnelFrame::BrowserAcquireControl { .. }
        | TunnelFrame::BrowserResize { .. } => {
            let message = ServerControlMessage::Error {
                code: ErrorCode::Protocol,
                message: protocol_error_message(),
            };
            let text = match serde_json::to_string(&message) {
                Ok(text) => text,
                Err(error) => {
                    debug!(%error, "failed to serialize invalid tunnel direction error");
                    return SendBrowserFrameResult::Continue(Ok(()));
                }
            };
            if let Err(error) = sender.send(Message::Text(text.into())).await {
                return SendBrowserFrameResult::Closed(Err(error));
            }
            SendBrowserFrameResult::Closed(
                sender.send(Message::Close(Some(protocol_close()))).await,
            )
        }
    }
}

async fn send_process_exited(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
) -> Result<(), axum::Error> {
    let message = ServerControlMessage::ProcessExited {
        message: SafeMessage::from_static("The terminal process exited."),
    };
    let text = match serde_json::to_string(&message) {
        Ok(text) => text,
        Err(error) => {
            debug!(%error, "failed to serialize process-exited control frame");
            return Ok(());
        }
    };
    sender.send(Message::Text(text.into())).await
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

async fn send_runtime_error(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
) -> Result<(), axum::Error> {
    let message = ServerControlMessage::Error {
        code: ErrorCode::Runtime,
        message: SafeMessage::from_static("backend operation failed"),
    };
    send_server_control_result(sender, &message).await
}

async fn send_forbidden_error(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
) -> Result<(), axum::Error> {
    let message = ServerControlMessage::Error {
        code: ErrorCode::Forbidden,
        message: SafeMessage::from_static("controller does not own input lease"),
    };
    send_server_control_result(sender, &message).await
}

async fn send_server_control(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: ServerControlMessage,
) {
    let _result = send_server_control_result(sender, &message).await;
}

async fn send_server_control_result(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: &ServerControlMessage,
) -> Result<(), axum::Error> {
    let text = match serde_json::to_string(message) {
        Ok(text) => text,
        Err(error) => {
            debug!(%error, "failed to serialize server control message");
            return Ok(());
        }
    };
    sender.send(Message::Text(text.into())).await
}

async fn send_server_ping(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
) -> Result<(), axum::Error> {
    sender
        .send(Message::Ping(Bytes::from_static(SERVER_PING_PAYLOAD)))
        .await
}

fn controller_from_client(client_id: ClientId) -> Result<ControllerRef, OperationLockError> {
    Ok(ControllerRef::new(
        ControllerKind::Browser,
        ControllerId::new(client_id.get())?,
    ))
}

fn screen_snapshot_bytes(
    snapshot: &BackendScreenSnapshot,
    viewport: Option<GatewayViewport>,
) -> Bytes {
    let viewport = viewport.unwrap_or_else(|| GatewayViewport::new(snapshot.size()));
    let visible_rows = viewport.size().rows.get();
    let visible_cols = viewport.size().cols.get();
    let snapshot_rows = snapshot.size().rows.get();
    let snapshot_cols = snapshot.size().cols.get();
    let backend_cursor_row = snapshot.cursor_row().min(snapshot_rows.saturating_sub(1));
    let backend_cursor_col = snapshot.cursor_col().min(snapshot_cols.saturating_sub(1));
    let start_row = snapshot_start_axis(
        snapshot_rows,
        backend_cursor_row,
        visible_rows,
        viewport.origin_row(),
    );
    let start_col = snapshot_start_axis(
        snapshot_cols,
        backend_cursor_col,
        visible_cols,
        viewport.origin_col(),
    );
    let cursor_row = backend_cursor_row
        .saturating_sub(start_row)
        .min(visible_rows.saturating_sub(1))
        .saturating_add(1);
    let cursor_col = backend_cursor_col
        .saturating_sub(start_col)
        .min(visible_cols.saturating_sub(1))
        .saturating_add(1);
    let mut text = String::from("\x1b[0m\x1b[?7l\x1b[H\x1b[2J");
    for index in 0..visible_rows {
        if index > 0 {
            text.push_str("\r\n");
        }
        let row = start_row.saturating_add(index);
        if let Some(line) = snapshot.lines().get(usize::from(row)) {
            text.push_str(&project_snapshot_line(line, start_col, visible_cols));
        }
    }
    let _result = write!(text, "\x1b[0m\x1b[?7h\x1b[{cursor_row};{cursor_col}H");
    Bytes::from(text)
}

const fn snapshot_start_axis(
    total_cells: u16,
    cursor_cell: u16,
    visible_cells: u16,
    requested_origin: Option<u16>,
) -> u16 {
    if total_cells <= visible_cells {
        return 0;
    }
    let max_start = total_cells.saturating_sub(visible_cells);
    if let Some(requested_origin) = requested_origin {
        return if requested_origin > max_start {
            max_start
        } else {
            requested_origin
        };
    }
    if cursor_cell >= max_start {
        return max_start;
    }
    if cursor_cell < visible_cells {
        return 0;
    }
    cursor_cell.saturating_add(1).saturating_sub(visible_cells)
}

fn project_snapshot_line(line: &str, start_col: u16, visible_cols: u16) -> String {
    let end_col = start_col.saturating_add(visible_cols);
    let mut output = String::new();
    let mut byte_index = 0;
    let mut cell_col = 0_u16;
    while byte_index < line.len() {
        let Some(rest) = line.get(byte_index..) else {
            break;
        };
        let Some(character) = rest.chars().next() else {
            break;
        };
        if character == '\x1b' {
            let sequence_end = ansi_sequence_end(line.as_bytes(), byte_index);
            if cell_col <= end_col
                && let Some(sequence) = line.get(byte_index..sequence_end)
            {
                output.push_str(sequence);
            }
            byte_index = sequence_end;
            continue;
        }

        let width = character_cell_width(character, cell_col);
        let next_cell_col = cell_col.saturating_add(width);
        if character_overlaps_viewport(cell_col, next_cell_col, start_col, end_col) {
            output.push(character);
        }
        cell_col = next_cell_col;
        byte_index = byte_index.saturating_add(character.len_utf8());
    }
    output
}

fn ansi_sequence_end(bytes: &[u8], start: usize) -> usize {
    let Some(kind) = bytes.get(start.saturating_add(1)) else {
        return bytes.len();
    };
    match *kind {
        b'[' => csi_sequence_end(bytes, start.saturating_add(2)),
        b']' => osc_sequence_end(bytes, start.saturating_add(2)),
        b'(' | b')' | b'*' | b'+' => bytes.len().min(start.saturating_add(3)),
        _ => bytes.len().min(start.saturating_add(2)),
    }
}

fn csi_sequence_end(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() {
        if let Some(byte) = bytes.get(index)
            && (0x40..=0x7e).contains(byte)
        {
            return index.saturating_add(1);
        }
        index = index.saturating_add(1);
    }
    bytes.len()
}

fn osc_sequence_end(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() {
        match bytes.get(index) {
            Some(0x07) => return index.saturating_add(1),
            Some(0x1b) if bytes.get(index.saturating_add(1)) == Some(&b'\\') => {
                return index.saturating_add(2);
            }
            Some(_byte) => {
                index = index.saturating_add(1);
            }
            None => return bytes.len(),
        }
    }
    bytes.len()
}

const fn character_overlaps_viewport(
    start: u16,
    end: u16,
    viewport_start: u16,
    viewport_end: u16,
) -> bool {
    if start == end {
        return start >= viewport_start && start < viewport_end;
    }
    end > viewport_start && start < viewport_end
}

fn character_cell_width(character: char, cell_col: u16) -> u16 {
    if character == '\t' {
        return 8_u16.saturating_sub(cell_col % 8);
    }
    if character.is_control() || is_combining_mark(character) {
        return 0;
    }
    if is_wide_character(character) {
        return 2;
    }
    1
}

const fn is_combining_mark(character: char) -> bool {
    matches!(
        character as u32,
        0x0300..=0x036f
            | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff
            | 0x20d0..=0x20ff
            | 0xfe20..=0xfe2f
    )
}

const fn is_wide_character(character: char) -> bool {
    matches!(
        character as u32,
        0x1100..=0x115f
            | 0x2329..=0x232a
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe19
            | 0xfe30..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f300..=0x1f64f
            | 0x1f900..=0x1f9ff
    )
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

fn runtime_unavailable_close() -> CloseFrame {
    CloseFrame {
        code: close_code::NORMAL,
        reason: CLOSE_REASON_SESSION_ENDED.into(),
    }
}

fn client_timeout_close() -> CloseFrame {
    CloseFrame {
        code: close_code::NORMAL,
        reason: "client heartbeat timeout".into(),
    }
}

fn close_frame_for_tunnel_reason(reason: TunnelCloseReason) -> CloseFrame {
    let reason = match reason {
        TunnelCloseReason::Supervisor => CLOSE_REASON_SERVER_SHUTDOWN,
        TunnelCloseReason::ClientDisconnect => CLOSE_REASON_CLIENT_DISCONNECTED,
        TunnelCloseReason::ControllerReplaced => CLOSE_REASON_CONTROLLER_REPLACED,
        TunnelCloseReason::ChildExit => CLOSE_REASON_SESSION_ENDED,
        TunnelCloseReason::RuntimeError(_error) => CLOSE_REASON_RUNTIME_ERROR,
    };
    CloseFrame {
        code: close_code::NORMAL,
        reason: reason.into(),
    }
}

fn validate_api_request(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
    query: &TokenQuery,
) -> Result<(), ApiError> {
    validate_http_request(state, peer, headers, query, false).map_err(ApiError::Security)
}

fn gateway_for_session(
    state: &AppState,
    requested: &SessionName,
) -> Result<Arc<Mutex<SessionGateway<TmuxBackend>>>, ApiError> {
    let WebSession::TmuxGateway { gateway, session } = &state.inner.session else {
        return Err(ApiError::Unsupported);
    };
    if session != requested {
        return Err(ApiError::NotFound);
    }
    Ok(Arc::clone(gateway))
}

fn agent_controller(id: u64) -> Result<ControllerRef, ApiError> {
    Ok(ControllerRef::new(
        ControllerKind::Agent,
        ControllerId::new(id).map_err(|_error| ApiError::BadRequest)?,
    ))
}

#[derive(Debug)]
struct WaitForScreenTextResult {
    matched: bool,
    snapshot: Option<BackendScreenSnapshot>,
}

fn bounded_semantic_text(text: String) -> Result<String, ApiError> {
    if text.len() > SEMANTIC_TEXT_MAX_BYTES || text.as_bytes().contains(&0) {
        return Err(ApiError::BadRequest);
    }
    Ok(text)
}

fn semantic_command_text(command: String) -> Result<String, ApiError> {
    if command.contains(['\r', '\n']) {
        return Err(ApiError::BadRequest);
    }
    bounded_semantic_text(command)
}

fn bounded_wait_text(text: String) -> Result<String, ApiError> {
    if text.is_empty() || text.len() > SEMANTIC_WAIT_TEXT_MAX_BYTES || text.as_bytes().contains(&0)
    {
        return Err(ApiError::BadRequest);
    }
    Ok(text)
}

fn semantic_wait_timeout(value: Option<u64>) -> Result<Duration, ApiError> {
    let millis = value.unwrap_or(SEMANTIC_WAIT_TIMEOUT_DEFAULT_MS);
    if millis == 0 || millis > SEMANTIC_WAIT_TIMEOUT_MAX_MS {
        return Err(ApiError::BadRequest);
    }
    Ok(Duration::from_millis(millis))
}

fn semantic_key_token(key: &str) -> Result<String, ApiError> {
    if key.is_empty() || key.len() > SEMANTIC_KEY_MAX_BYTES {
        return Err(ApiError::BadRequest);
    }
    let token = match key {
        "Enter" => "Enter",
        "Tab" => "Tab",
        "Escape" => "Escape",
        "Backspace" => "BSpace",
        "CtrlC" => "C-c",
        "CtrlD" => "C-d",
        "ArrowUp" => "Up",
        "ArrowDown" => "Down",
        "ArrowRight" => "Right",
        "ArrowLeft" => "Left",
        _ => {
            if key.chars().count() != 1 || key.chars().any(char::is_control) {
                return Err(ApiError::BadRequest);
            }
            key
        }
    };
    Ok(token.to_owned())
}

async fn send_semantic_text(
    state: &AppState,
    session: &SessionName,
    controller_id: u64,
    text: &str,
) -> Result<(), ApiError> {
    let controller = agent_controller(controller_id)?;
    let gateway = gateway_for_session(state, session)?;
    let mut gateway = gateway.lock().await;
    gateway
        .send_text(session, controller, text, Instant::now())
        .await
        .map_err(ApiError::from_gateway)?;
    Ok(())
}

async fn send_semantic_key(
    state: &AppState,
    session: &SessionName,
    controller_id: u64,
    key: &str,
) -> Result<(), ApiError> {
    let controller = agent_controller(controller_id)?;
    let gateway = gateway_for_session(state, session)?;
    let mut gateway = gateway.lock().await;
    gateway
        .send_key(session, controller, key, Instant::now())
        .await
        .map_err(ApiError::from_gateway)?;
    Ok(())
}

async fn wait_for_screen_text(
    gateway: &Arc<Mutex<SessionGateway<TmuxBackend>>>,
    session: &SessionName,
    needle: &str,
    timeout: Duration,
) -> Result<WaitForScreenTextResult, ApiError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(ApiError::BadRequest)?;
    loop {
        let snapshot = {
            let mut gateway = gateway.lock().await;
            gateway
                .read_screen(session)
                .await
                .map_err(ApiError::from_gateway)?
        };
        if snapshot.lines().iter().any(|line| line.contains(needle)) {
            return Ok(WaitForScreenTextResult {
                matched: true,
                snapshot: Some(snapshot),
            });
        }
        if Instant::now() >= deadline {
            return Ok(WaitForScreenTextResult {
                matched: false,
                snapshot: Some(snapshot),
            });
        }
        time::sleep(SEMANTIC_WAIT_POLL_INTERVAL).await;
    }
}

async fn read_gateway_screen(
    gateway: &Arc<Mutex<SessionGateway<TmuxBackend>>>,
    session: &SessionName,
) -> Result<ScreenResponse, ApiError> {
    let snapshot = {
        let mut gateway = gateway.lock().await;
        gateway
            .read_screen(session)
            .await
            .map_err(ApiError::from_gateway)?
    };
    Ok(screen_response(&snapshot))
}

fn screen_response(snapshot: &BackendScreenSnapshot) -> ScreenResponse {
    ScreenResponse {
        size: snapshot.size(),
        cursor_col: snapshot.cursor_col(),
        cursor_row: snapshot.cursor_row(),
        lines: snapshot.lines().to_vec(),
    }
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

#[derive(Debug)]
enum ApiError {
    BadRequest,
    Conflict,
    Forbidden,
    NotFound,
    Runtime,
    Security(WebError),
    Unsupported,
}

impl ApiError {
    fn from_gateway(error: SessionGatewayError) -> Self {
        match error {
            SessionGatewayError::Lock(OperationLockError::LeaseConflict { .. }) => Self::Conflict,
            SessionGatewayError::Lock(OperationLockError::NotLeaseOwner { .. }) => Self::Forbidden,
            SessionGatewayError::Lock(_error) => Self::BadRequest,
            SessionGatewayError::Registry(_error) => Self::NotFound,
            SessionGatewayError::Backend(_error) => Self::Runtime,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Security(error) => error.into_response(),
            error => {
                debug!(reason = ?error, "semantic api request failed");
                let (status, code, message) = match error {
                    Self::BadRequest => (
                        StatusCode::BAD_REQUEST,
                        ErrorCode::Protocol,
                        SafeMessage::from_static("invalid semantic operation request"),
                    ),
                    Self::Conflict => (
                        StatusCode::CONFLICT,
                        ErrorCode::Forbidden,
                        SafeMessage::from_static("operation lease is owned by another controller"),
                    ),
                    Self::Forbidden => (
                        StatusCode::FORBIDDEN,
                        ErrorCode::Forbidden,
                        SafeMessage::from_static("controller does not own input lease"),
                    ),
                    Self::NotFound => (
                        StatusCode::NOT_FOUND,
                        ErrorCode::Runtime,
                        SafeMessage::from_static("session was not found"),
                    ),
                    Self::Runtime => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        ErrorCode::Runtime,
                        SafeMessage::from_static("backend operation failed"),
                    ),
                    Self::Unsupported => (
                        StatusCode::CONFLICT,
                        ErrorCode::Runtime,
                        SafeMessage::from_static("semantic api requires a backend session"),
                    ),
                    Self::Security(_error) => {
                        return StatusCode::FORBIDDEN.into_response();
                    }
                };
                let body = ServerControlMessage::Error { code, message };
                (status, Json(body)).into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        net::{IpAddr, Ipv4Addr},
        path::PathBuf,
        process::Stdio,
    };

    use anyhow::Context;
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode},
    };
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use serde::de::DeserializeOwned;
    use serde_json::json;
    use termstage_core::{
        protocol::{AccessToken, SessionName, TerminalSize},
        runtime::{
            ExitPolicy, ReconnectPolicy, RuntimeSession, SessionMode, ShellCommand, ShutdownReason,
        },
        session_gateway::SessionGateway,
        tmux_backend::TmuxBackend,
    };
    use tokio::{
        process::Command as TokioCommand,
        time::{Duration, sleep, timeout},
    };
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Message as TungsteniteMessage, client::IntoClientRequest},
    };
    use tower::ServiceExt;

    use super::*;

    fn test_shell_command() -> anyhow::Result<ShellCommand> {
        ShellCommand::new(
            "/bin/bash",
            [OsString::from("--noprofile"), OsString::from("--norc")],
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
            exit_policy: ExitPolicy::Hold,
        };
        let mut config = WebConfig::local(token.clone(), commands, runtime);
        config.port = 49152;
        Ok((config, token))
    }

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

    async fn api_post_json(
        app: &Router,
        path: &str,
        token: &AccessToken,
        body: serde_json::Value,
    ) -> anyhow::Result<Response> {
        let uri = format!("{path}?token={}", token.to_url_token());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .header(header::HOST, "127.0.0.1:49152")
                    .header(header::CONTENT_TYPE, "application/json")
                    .extension(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 50000))))
                    .body(Body::from(serde_json::to_vec(&body)?))?,
            )
            .await?;
        Ok(response)
    }

    async fn api_post_empty(
        app: &Router,
        path: &str,
        token: &AccessToken,
    ) -> anyhow::Result<Response> {
        let uri = format!("{path}?token={}", token.to_url_token());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .header(header::HOST, "127.0.0.1:49152")
                    .extension(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 50000))))
                    .body(Body::empty())?,
            )
            .await?;
        Ok(response)
    }

    async fn decode_json<T>(response: Response) -> anyhow::Result<T>
    where
        T: DeserializeOwned,
    {
        let body = response.into_body().collect().await?.to_bytes();
        serde_json::from_slice(&body).map_err(Into::into)
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
            cols: None,
            rows: None,
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
            cols: None,
            rows: None,
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
            exit_policy: ExitPolicy::Hold,
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
    async fn test_should_bridge_websocket_to_tmux_gateway() -> anyhow::Result<()> {
        let backend = TmuxBackend::from_path().context("tmux unavailable")?;
        let tmux = backend.tmux_path().to_path_buf();
        let token = AccessToken::from_bytes([17; 32]);
        let session = SessionName::new(format!("termstage-gateway-ws-{}", std::process::id()))?;
        let _cleanup = TmuxSessionCleanup::new(tmux, session.clone());
        let mut gateway = SessionGateway::new(backend, Duration::from_secs(90));
        gateway
            .create_or_find_session(session.clone(), session.clone(), TerminalSize::new(80, 24)?)
            .await?;
        let config = WebConfig::local_tmux_gateway(
            token.clone(),
            Arc::new(Mutex::new(gateway)),
            session.clone(),
        );
        let server = serve(config).await?;
        let mut socket = connect_test_socket(server.address(), &token).await?;
        assert!(
            read_socket_text_until(&mut socket, "\"replayFinished\"").await?,
            "gateway websocket did not finish initial replay"
        );

        socket
            .send(TungsteniteMessage::Binary(Bytes::from_static(
                b"printf gateway-ws-ok\\n\n",
            )))
            .await?;

        assert!(
            read_socket_until(&mut socket, b"gateway-ws-ok").await?,
            "gateway websocket did not receive tmux backend output"
        );
        socket.close(None).await?;
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_ignore_browser_resize_for_tmux_gateway() -> anyhow::Result<()> {
        let backend = TmuxBackend::from_path().context("tmux unavailable")?;
        let tmux = backend.tmux_path().to_path_buf();
        let token = AccessToken::from_bytes([23; 32]);
        let session = SessionName::new(format!("termstage-gateway-resize-{}", std::process::id()))?;
        let _cleanup = TmuxSessionCleanup::new(tmux, session.clone());
        let mut gateway = SessionGateway::new(backend, Duration::from_secs(90));
        let initial_size = TerminalSize::new(80, 24)?;
        gateway
            .create_or_find_session(session.clone(), session.clone(), initial_size)
            .await?;
        let shared_gateway = Arc::new(Mutex::new(gateway));
        let config = WebConfig::local_tmux_gateway(
            token.clone(),
            Arc::clone(&shared_gateway),
            session.clone(),
        );
        let server = serve(config).await?;
        let mut socket = connect_test_socket(server.address(), &token).await?;
        assert!(
            read_socket_text_until(&mut socket, "\"replayFinished\"").await?,
            "gateway websocket did not finish initial replay"
        );
        let browser_size = TerminalSize::new(101, 33)?;
        let text = serde_json::to_string(&ClientControlMessage::Resize {
            cols: browser_size.cols,
            rows: browser_size.rows,
        })?;

        socket.send(TungsteniteMessage::Text(text.into())).await?;
        assert!(
            read_socket_text_until(&mut socket, r#""size":{"cols":101,"rows":33}"#).await?,
            "gateway websocket did not ack browser-local resize"
        );
        sleep(Duration::from_millis(150)).await;
        let snapshot = {
            let mut gateway = shared_gateway.lock().await;
            gateway.read_screen(&session).await?
        };

        assert_eq!(snapshot.size(), initial_size);
        socket.close(None).await?;
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_close_gateway_socket_when_tmux_session_ends() -> anyhow::Result<()> {
        let backend = TmuxBackend::from_path().context("tmux unavailable")?;
        let tmux = backend.tmux_path().to_path_buf();
        let token = AccessToken::from_bytes([24; 32]);
        let session = SessionName::new(format!("termstage-gateway-end-{}", std::process::id()))?;
        let mut cleanup = TmuxSessionCleanup::new(tmux.clone(), session.clone());
        let mut gateway = SessionGateway::new(backend, Duration::from_secs(90));
        gateway
            .create_or_find_session(session.clone(), session.clone(), TerminalSize::new(80, 24)?)
            .await?;
        let config = WebConfig::local_tmux_gateway(
            token.clone(),
            Arc::new(Mutex::new(gateway)),
            session.clone(),
        );
        let server = serve(config).await?;
        let mut socket = connect_test_socket(server.address(), &token).await?;
        assert!(
            read_socket_text_until(&mut socket, "\"replayFinished\"").await?,
            "gateway websocket did not finish initial replay"
        );
        let status = TokioCommand::new(&tmux)
            .env_remove("TMUX")
            .args(["kill-session", "-t", session.as_str()])
            .status()
            .await
            .context("failed to kill tmux test session")?;
        if !status.success() {
            anyhow::bail!("tmux kill-session exited with {status}");
        }
        cleanup.active = false;

        let close_reason = read_socket_close_reason(&mut socket).await?;
        assert_eq!(close_reason.as_deref(), Some(CLOSE_REASON_SESSION_ENDED));
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_operate_tmux_gateway_with_semantic_api() -> anyhow::Result<()> {
        let backend = TmuxBackend::from_path().context("tmux unavailable")?;
        let tmux = backend.tmux_path().to_path_buf();
        let token = AccessToken::from_bytes([18; 32]);
        let session = SessionName::new(format!("termstage-semantic-api-{}", std::process::id()))?;
        let _cleanup = TmuxSessionCleanup::new(tmux, session.clone());
        let mut gateway = SessionGateway::new(backend, Duration::from_secs(90));
        gateway
            .create_or_find_session(session.clone(), session.clone(), TerminalSize::new(80, 24)?)
            .await?;
        let mut config = WebConfig::local_tmux_gateway(
            token.clone(),
            Arc::new(Mutex::new(gateway)),
            session.clone(),
        );
        config.port = 49152;
        let app = router(config)?;
        let base = format!("/api/sessions/{}", session.as_str());

        let response = api_post_json(
            &app,
            &format!("{base}/acquire-lock"),
            &token,
            json!({ "controllerId": 42 }),
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let lease: LeaseResponse = decode_json(response).await?;
        assert_eq!(lease.owner, LeaseOwner::Agent);

        let response = api_post_json(
            &app,
            &format!("{base}/write-text"),
            &token,
            json!({ "controllerId": 42, "text": "printf semantic-write-ok" }),
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let response = api_post_json(
            &app,
            &format!("{base}/press-key"),
            &token,
            json!({ "controllerId": 42, "key": "Enter" }),
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            read_semantic_screen_until(&app, &base, &token, "semantic-write-ok").await?,
            "semantic write-text/press-key output was not visible"
        );

        let response = api_post_json(
            &app,
            &format!("{base}/run-command"),
            &token,
            json!({
                "controllerId": 42,
                "command": "printf semantic-run-ok",
                "waitFor": "semantic-run-ok",
                "capture": true
            }),
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let run: RunCommandResponse = decode_json(response).await?;
        assert_eq!(run.matched, Some(true));
        assert!(
            run.screen.as_ref().is_some_and(|screen| screen
                .lines
                .iter()
                .any(|line| line.contains("semantic-run-ok"))),
            "semantic run-command response did not capture the matched screen"
        );
        assert!(
            read_semantic_screen_until(&app, &base, &token, "semantic-run-ok").await?,
            "semantic run-command output was not visible"
        );

        let response = api_post_json(
            &app,
            &format!("{base}/scroll"),
            &token,
            json!({ "controllerId": 42, "direction": "up", "amount": 1 }),
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);

        let response = api_post_json(
            &app,
            &format!("{base}/release-lock"),
            &token,
            json!({ "controllerId": 42 }),
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_reject_semantic_write_from_non_owner() -> anyhow::Result<()> {
        let backend = TmuxBackend::from_path().context("tmux unavailable")?;
        let tmux = backend.tmux_path().to_path_buf();
        let token = AccessToken::from_bytes([19; 32]);
        let session = SessionName::new(format!("termstage-semantic-lock-{}", std::process::id()))?;
        let _cleanup = TmuxSessionCleanup::new(tmux, session.clone());
        let mut gateway = SessionGateway::new(backend, Duration::from_secs(90));
        gateway
            .create_or_find_session(session.clone(), session.clone(), TerminalSize::new(80, 24)?)
            .await?;
        let mut config = WebConfig::local_tmux_gateway(
            token.clone(),
            Arc::new(Mutex::new(gateway)),
            session.clone(),
        );
        config.port = 49152;
        let app = router(config)?;
        let base = format!("/api/sessions/{}", session.as_str());

        let response = api_post_json(
            &app,
            &format!("{base}/acquire-lock"),
            &token,
            json!({ "controllerId": 1 }),
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let response = api_post_json(
            &app,
            &format!("{base}/write-text"),
            &token,
            json!({ "controllerId": 2, "text": "blocked" }),
        )
        .await?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_sync_agent_api_output_to_read_only_browser() -> anyhow::Result<()> {
        let backend = TmuxBackend::from_path().context("tmux unavailable")?;
        let tmux = backend.tmux_path().to_path_buf();
        let token = AccessToken::from_bytes([20; 32]);
        let session = SessionName::new(format!(
            "termstage-agent-browser-sync-{}",
            std::process::id()
        ))?;
        let _cleanup = TmuxSessionCleanup::new(tmux, session.clone());
        let mut gateway = SessionGateway::new(backend, Duration::from_secs(90));
        gateway
            .create_or_find_session(session.clone(), session.clone(), TerminalSize::new(80, 24)?)
            .await?;
        let shared_gateway = Arc::new(Mutex::new(gateway));
        let mut app_config = WebConfig::local_tmux_gateway(
            token.clone(),
            Arc::clone(&shared_gateway),
            session.clone(),
        );
        app_config.port = 49152;
        let app = router(app_config)?;
        let server_config =
            WebConfig::local_tmux_gateway(token.clone(), shared_gateway, session.clone());
        let server = serve(server_config).await?;
        let base = format!("/api/sessions/{}", session.as_str());

        let response = api_post_json(
            &app,
            &format!("{base}/acquire-lock"),
            &token,
            json!({ "controllerId": 77 }),
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let response = api_post_json(
            &app,
            &format!("{base}/run-command"),
            &token,
            json!({
                "controllerId": 77,
                "command": "printf agent-to-browser-sync-ok",
                "waitFor": "agent-to-browser-sync-ok"
            }),
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let run: RunCommandResponse = decode_json(response).await?;
        assert_eq!(run.matched, Some(true));

        let mut socket = connect_test_socket(server.address(), &token).await?;
        assert!(
            read_socket_until(&mut socket, b"agent-to-browser-sync-ok").await?,
            "read-only browser did not receive Agent API output"
        );
        socket
            .send(TungsteniteMessage::Binary(Bytes::from_static(
                b"printf forbidden-browser-write\\n\n",
            )))
            .await?;
        assert!(
            read_socket_text_until(&mut socket, "\"code\":\"forbidden\"").await?,
            "read-only browser write did not receive forbidden control error"
        );
        socket.close(None).await?;
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_reject_agent_lock_when_browser_owns_gateway() -> anyhow::Result<()> {
        let backend = TmuxBackend::from_path().context("tmux unavailable")?;
        let tmux = backend.tmux_path().to_path_buf();
        let token = AccessToken::from_bytes([21; 32]);
        let session = SessionName::new(format!(
            "termstage-browser-agent-lock-{}",
            std::process::id()
        ))?;
        let _cleanup = TmuxSessionCleanup::new(tmux, session.clone());
        let mut gateway = SessionGateway::new(backend, Duration::from_secs(90));
        gateway
            .create_or_find_session(session.clone(), session.clone(), TerminalSize::new(80, 24)?)
            .await?;
        let shared_gateway = Arc::new(Mutex::new(gateway));
        let mut app_config = WebConfig::local_tmux_gateway(
            token.clone(),
            Arc::clone(&shared_gateway),
            session.clone(),
        );
        app_config.port = 49152;
        let app = router(app_config)?;
        let server_config =
            WebConfig::local_tmux_gateway(token.clone(), shared_gateway, session.clone());
        let server = serve(server_config).await?;
        let mut socket = connect_test_socket(server.address(), &token).await?;
        assert!(
            read_socket_text_until(&mut socket, "\"type\":\"leaseChanged\"").await?,
            "browser did not acquire a gateway lease"
        );

        let base = format!("/api/sessions/{}", session.as_str());
        let response = api_post_json(
            &app,
            &format!("{base}/acquire-lock"),
            &token,
            json!({ "controllerId": 88 }),
        )
        .await?;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        socket.close(None).await?;
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_sync_native_tmux_writes_to_browser_and_api() -> anyhow::Result<()> {
        let backend = TmuxBackend::from_path().context("tmux unavailable")?;
        let tmux = backend.tmux_path().to_path_buf();
        let token = AccessToken::from_bytes([22; 32]);
        let session = SessionName::new(format!("termstage-native-sync-{}", std::process::id()))?;
        let _cleanup = TmuxSessionCleanup::new(tmux.clone(), session.clone());
        let mut gateway = SessionGateway::new(backend, Duration::from_secs(90));
        gateway
            .create_or_find_session(session.clone(), session.clone(), TerminalSize::new(80, 24)?)
            .await?;
        let shared_gateway = Arc::new(Mutex::new(gateway));
        let mut app_config = WebConfig::local_tmux_gateway(
            token.clone(),
            Arc::clone(&shared_gateway),
            session.clone(),
        );
        app_config.port = 49152;
        let app = router(app_config)?;
        let server_config =
            WebConfig::local_tmux_gateway(token.clone(), shared_gateway, session.clone());
        let server = serve(server_config).await?;
        let mut socket = connect_test_socket(server.address(), &token).await?;

        send_native_tmux_input(&tmux, &session, "printf native-tmux-sync-ok\\n\n").await?;
        assert!(
            read_socket_until(&mut socket, b"native-tmux-sync-ok").await?,
            "browser did not receive output written through native tmux path"
        );
        let base = format!("/api/sessions/{}", session.as_str());
        assert!(
            read_semantic_screen_until(&app, &base, &token, "native-tmux-sync-ok").await?,
            "semantic API did not read output written through native tmux path"
        );
        socket.close(None).await?;
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_forward_websocket_resize_through_tunnel_bridge() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([16; 32]);
        let (commands, mut command_rx) = mpsc::channel(8);
        let runtime = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: TerminalSize::new(80, 24)?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
            exit_policy: ExitPolicy::Hold,
        };
        let server = serve(WebConfig::local(token.clone(), commands, runtime)).await?;
        let mut socket = connect_test_socket(server.address(), &token).await?;
        let Some(RuntimeCommand::AttachClient { client_id, output }) =
            timeout(Duration::from_secs(5), command_rx.recv()).await?
        else {
            anyhow::bail!("expected attach command");
        };
        let size = TerminalSize::new(101, 33)?;
        let text = serde_json::to_string(&ClientControlMessage::Resize {
            cols: size.cols,
            rows: size.rows,
        })?;

        socket.send(TungsteniteMessage::Text(text.into())).await?;
        let Some(RuntimeCommand::BrowserResize {
            client_id: resized_id,
            size: resized_size,
        }) = timeout(Duration::from_secs(5), command_rx.recv()).await?
        else {
            anyhow::bail!("expected browser resize command");
        };

        assert_eq!(resized_id, client_id);
        assert_eq!(resized_size, size);
        drop(output);
        socket.close(None).await?;
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_forward_websocket_acquire_control_through_tunnel_bridge()
    -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([25; 32]);
        let (commands, mut command_rx) = mpsc::channel(8);
        let runtime = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: TerminalSize::new(80, 24)?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
            exit_policy: ExitPolicy::Hold,
        };
        let server = serve(WebConfig::local(token.clone(), commands, runtime)).await?;
        let mut socket = connect_test_socket(server.address(), &token).await?;
        let Some(RuntimeCommand::AttachClient { client_id, output }) =
            timeout(Duration::from_secs(5), command_rx.recv()).await?
        else {
            anyhow::bail!("expected attach command");
        };
        let text = serde_json::to_string(&ClientControlMessage::AcquireControl)?;

        socket.send(TungsteniteMessage::Text(text.into())).await?;
        let Some(RuntimeCommand::AcquireControl {
            client_id: acquired_id,
        }) = timeout(Duration::from_secs(5), command_rx.recv()).await?
        else {
            anyhow::bail!("expected browser acquire-control command");
        };

        assert_eq!(acquired_id, client_id);
        drop(output);
        socket.close(None).await?;
        server.shutdown().await?;
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
            exit_policy: ExitPolicy::Hold,
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
    async fn test_should_send_process_exited_control_before_child_exit_close() -> anyhow::Result<()>
    {
        let token = AccessToken::from_bytes([14; 32]);
        let runtime = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: TerminalSize::new(80, 24)?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
            exit_policy: ExitPolicy::End,
        };
        let session = RuntimeSession::start(runtime.clone())?;
        let config = WebConfig::local(token.clone(), session.command_sender(), runtime);
        let server = serve(config).await?;

        let mut socket = connect_test_socket(server.address(), &token).await?;
        socket
            .send(TungsteniteMessage::Binary(Bytes::from_static(b"exit\n")))
            .await?;
        assert!(
            read_socket_text_until(&mut socket, "\"type\":\"processExited\"").await?,
            "websocket did not receive process-exited control before close"
        );
        let close_reason = read_socket_close_reason(&mut socket).await?;
        assert_eq!(close_reason.as_deref(), Some(CLOSE_REASON_SESSION_ENDED));
        server.shutdown().await?;
        drop(session);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_replace_live_websocket_controller() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([11; 32]);
        let runtime = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: TerminalSize::new(80, 24)?,
            reconnect_policy: ReconnectPolicy::KeepAlive,
            exit_policy: ExitPolicy::Hold,
        };
        let session = RuntimeSession::start(runtime.clone())?;
        let config = WebConfig::local(token.clone(), session.command_sender(), runtime);
        let server = serve(config).await?;

        let mut first = connect_test_socket(server.address(), &token).await?;
        first
            .send(TungsteniteMessage::Binary(Bytes::from_static(
                b"printf controller-before-replace\\n\n",
            )))
            .await?;
        assert!(
            read_socket_until(&mut first, b"controller-before-replace").await?,
            "first websocket did not receive terminal output"
        );

        let mut second = connect_test_socket(server.address(), &token).await?;
        let first_close_reason = read_socket_close_reason(&mut first).await?;
        assert_eq!(first_close_reason.as_deref(), Some("controller replaced"));
        second
            .send(TungsteniteMessage::Binary(Bytes::from_static(
                b"printf controller-after-replace\\n\n",
            )))
            .await?;
        assert!(
            read_socket_until(&mut second, b"controller-after-replace").await?,
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
            exit_policy: ExitPolicy::Hold,
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
            exit_policy: ExitPolicy::Hold,
        };
        let server = serve(WebConfig::local(token.clone(), commands, runtime)).await?;
        let runtime_task = tokio::spawn(async move {
            if let Some(RuntimeCommand::AttachClient { output, .. }) = command_rx.recv().await {
                drop(output);
            }
        });

        let mut socket = connect_test_socket(server.address(), &token).await?;
        let close_reason = read_socket_close_reason(&mut socket).await?;
        assert_eq!(close_reason.as_deref(), Some(CLOSE_REASON_SESSION_ENDED));
        runtime_task.await.context("fake runtime task panicked")?;
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_close_websocket_when_runtime_is_unavailable_on_attach()
    -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([12; 32]);
        let (commands, command_rx) = mpsc::channel(8);
        drop(command_rx);
        let runtime = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: TerminalSize::new(80, 24)?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
            exit_policy: ExitPolicy::Hold,
        };
        let server = serve(WebConfig::local(token.clone(), commands, runtime)).await?;

        let mut socket = connect_test_socket(server.address(), &token).await?;
        let close_reason = read_socket_close_reason(&mut socket).await?;
        assert_eq!(close_reason.as_deref(), Some(CLOSE_REASON_SESSION_ENDED));
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_should_close_websocket_when_runtime_stops_after_attach() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([13; 32]);
        let (commands, mut command_rx) = mpsc::channel(8);
        let runtime = RuntimeConfig {
            mode: SessionMode::NewShell {
                shell: test_shell_command()?,
            },
            initial_size: TerminalSize::new(80, 24)?,
            reconnect_policy: ReconnectPolicy::TerminateOnShutdown,
            exit_policy: ExitPolicy::Hold,
        };
        let server = serve(WebConfig::local(token.clone(), commands, runtime)).await?;
        let runtime_task = tokio::spawn(async move {
            if let Some(RuntimeCommand::AttachClient { output, .. }) = command_rx.recv().await {
                drop(output);
            }
        });

        let mut socket = connect_test_socket(server.address(), &token).await?;
        socket
            .send(TungsteniteMessage::Binary(Bytes::from_static(b"x")))
            .await?;
        let close_reason = read_socket_close_reason(&mut socket).await?;
        assert_eq!(close_reason.as_deref(), Some(CLOSE_REASON_SESSION_ENDED));
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
            exit_policy: ExitPolicy::Hold,
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

    #[test]
    fn test_should_render_gateway_snapshot_without_forcing_visible_cursor_to_bottom()
    -> anyhow::Result<()> {
        let snapshot = BackendScreenSnapshot::new(
            TerminalSize::new(120, 40)?,
            10,
            10,
            numbered_snapshot_lines(40),
        );

        let viewport = GatewayViewport::new(TerminalSize::new(80, 24)?);
        let bytes = screen_snapshot_bytes(&snapshot, Some(viewport));
        let text = std::str::from_utf8(bytes.as_ref())?;

        assert!(text.starts_with("\u{1b}[0m\u{1b}[?7l\u{1b}[H\u{1b}[2Jrow-00"));
        assert!(text.contains("row-23"));
        assert!(!text.contains("row-24"));
        assert!(text.ends_with("\u{1b}[0m\u{1b}[?7h\u{1b}[11;11H"));
        Ok(())
    }

    #[test]
    fn test_should_render_gateway_snapshot_bottom_when_cursor_is_near_backend_bottom()
    -> anyhow::Result<()> {
        let snapshot = BackendScreenSnapshot::new(
            TerminalSize::new(120, 40)?,
            10,
            37,
            numbered_snapshot_lines(40),
        );

        let viewport = GatewayViewport::new(TerminalSize::new(80, 24)?);
        let bytes = screen_snapshot_bytes(&snapshot, Some(viewport));
        let text = std::str::from_utf8(bytes.as_ref())?;

        assert!(text.starts_with("\u{1b}[0m\u{1b}[?7l\u{1b}[H\u{1b}[2Jrow-16"));
        assert!(!text.contains("row-15"));
        assert!(text.contains("row-39"));
        assert!(text.ends_with("\u{1b}[0m\u{1b}[?7h\u{1b}[22;11H"));
        Ok(())
    }

    #[test]
    fn test_should_project_gateway_snapshot_horizontally() -> anyhow::Result<()> {
        let snapshot = BackendScreenSnapshot::new(
            TerminalSize::new(40, 24)?,
            35,
            3,
            vec!["0000000000111111111122222222223333333333".to_owned()],
        );

        let viewport = GatewayViewport::new(TerminalSize::new(20, 5)?);
        let bytes = screen_snapshot_bytes(&snapshot, Some(viewport));
        let text = std::str::from_utf8(bytes.as_ref())?;

        assert!(text.contains("22222222223333333333"));
        assert!(!text.contains("0000000000"));
        assert!(text.ends_with("\u{1b}[0m\u{1b}[?7h\u{1b}[4;16H"));
        Ok(())
    }

    #[test]
    fn test_should_project_gateway_snapshot_from_requested_origin() -> anyhow::Result<()> {
        let mut viewport = GatewayViewport::new(TerminalSize::new(20, 5)?);
        viewport.update_origin(Some(8), None);
        let snapshot = BackendScreenSnapshot::new(
            TerminalSize::new(40, 24)?,
            2,
            3,
            vec!["0123456789abcdefghijklmnopqrst".to_owned()],
        );

        let bytes = screen_snapshot_bytes(&snapshot, Some(viewport));
        let text = std::str::from_utf8(bytes.as_ref())?;

        assert!(text.contains("89abcdefghijklmnopq"));
        assert!(!text.contains("01234567"));
        assert!(!text.contains("rst"));
        assert!(text.ends_with("\u{1b}[0m\u{1b}[?7h\u{1b}[4;1H"));
        Ok(())
    }

    #[test]
    fn test_should_preserve_ansi_state_when_projecting_gateway_snapshot() -> anyhow::Result<()> {
        let mut viewport = GatewayViewport::new(TerminalSize::new(20, 5)?);
        viewport.update_origin(Some(4), None);
        let snapshot = BackendScreenSnapshot::new(
            TerminalSize::new(120, 24)?,
            5,
            0,
            vec!["\u{1b}[31m0123456789\u{1b}[0m".to_owned()],
        );

        let bytes = screen_snapshot_bytes(&snapshot, Some(viewport));
        let text = std::str::from_utf8(bytes.as_ref())?;

        assert!(text.contains("\u{1b}[31m456789"));
        assert!(!text.contains("0123"));
        assert!(text.ends_with("\u{1b}[0m\u{1b}[?7h\u{1b}[1;2H"));
        Ok(())
    }

    fn numbered_snapshot_lines(rows: u16) -> Vec<String> {
        (0..rows).map(|row| format!("row-{row:02}")).collect()
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

    async fn read_socket_text_until(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        needle: &str,
    ) -> anyhow::Result<bool> {
        timeout(Duration::from_secs(5), async {
            while let Some(message) = socket.next().await {
                match message? {
                    TungsteniteMessage::Text(text) => {
                        if text.contains(needle) {
                            return anyhow::Ok(true);
                        }
                    }
                    TungsteniteMessage::Close(_frame) => return anyhow::Ok(false),
                    TungsteniteMessage::Binary(_bytes) => {}
                    TungsteniteMessage::Ping(_bytes) => {}
                    TungsteniteMessage::Pong(_bytes) => {}
                    TungsteniteMessage::Frame(_frame) => {}
                }
            }
            anyhow::Ok(false)
        })
        .await?
    }

    async fn read_semantic_screen_until(
        app: &Router,
        base: &str,
        token: &AccessToken,
        needle: &str,
    ) -> anyhow::Result<bool> {
        timeout(Duration::from_secs(5), async {
            loop {
                let response = api_post_empty(app, &format!("{base}/read-screen"), token).await?;
                if response.status() != StatusCode::OK {
                    return anyhow::Ok(false);
                }
                let screen: ScreenResponse = decode_json(response).await?;
                if screen.lines.iter().any(|line| line.contains(needle)) {
                    return anyhow::Ok(true);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await?
    }

    async fn send_native_tmux_input(
        tmux: &PathBuf,
        session: &SessionName,
        input: &str,
    ) -> anyhow::Result<()> {
        let status = TokioCommand::new(tmux)
            .env_remove("TMUX")
            .args(["send-keys", "-t", session.as_str(), "-l", "--", input])
            .status()
            .await
            .context("failed to send native tmux input")?;
        if !status.success() {
            anyhow::bail!("native tmux send-keys exited with {status}");
        }
        Ok(())
    }

    async fn read_socket_close_reason(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> anyhow::Result<Option<String>> {
        timeout(Duration::from_secs(5), async {
            while let Some(message) = socket.next().await {
                match message? {
                    TungsteniteMessage::Close(frame) => {
                        return anyhow::Ok(frame.map(|frame| frame.reason.to_string()));
                    }
                    TungsteniteMessage::Binary(_)
                    | TungsteniteMessage::Text(_)
                    | TungsteniteMessage::Ping(_)
                    | TungsteniteMessage::Pong(_)
                    | TungsteniteMessage::Frame(_) => {}
                }
            }
            anyhow::Ok(None)
        })
        .await?
    }
}
