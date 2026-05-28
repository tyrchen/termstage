//! Command-line interface for browser terminal mode.

use std::{
    env,
    ffi::OsString,
    io::{self, Write},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    process,
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use termstage_core::{
    protocol::{AccessToken, SessionName, TerminalSize},
    runtime::{
        ExitPolicy, ReconnectPolicy, RuntimeConfig, RuntimeSession, SessionMode, ShellCommand,
        ShutdownReason,
    },
    security::{BasePath, PublicBaseUrl},
    session_gateway::SessionGateway,
    tmux_backend::{TmuxBackend, TmuxSessionCommand},
};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Mutex,
};
use tracing::{debug, info, warn};
use url::{Host, Url};

use crate::web::{PresentationSettings, PresentationTheme, WebConfig, WebExposure, serve};

const DEFAULT_SESSION: &str = "presentation";
const DEFAULT_FONT_SIZE: u16 = 24;
const MIN_FONT_SIZE: u16 = 12;
const MAX_FONT_SIZE: u16 = 96;
const TOKEN_ENV_MAX_BYTES: usize = 128;
const API_RESPONSE_MAX_BYTES: usize = 1024 * 1024;
const ACTOR_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const GATEWAY_LEASE_TTL: Duration = Duration::from_secs(90);
const GATEWAY_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const HTTP_HEADER_SEPARATOR: &[u8; 4] = b"\r\n\r\n";
const HTTP_LINE_SEPARATOR: &[u8; 2] = b"\r\n";
const SESSION_REGISTRY_VERSION: u16 = 1;
const SESSION_REGISTRY_MAX_BYTES: u64 = 1024 * 1024;
const SESSION_REGISTRY_MAX_RECORDS: usize = 1024;
const SESSION_LABEL_MAX_BYTES: usize = 64;
const SESSION_CREATED_AT_MAX_BYTES: usize = 64;
const SESSION_COMMAND_MAX_ARGS: usize = 256;
const SESSION_COMMAND_ARG_MAX_BYTES: usize = 4096;

/// Browser terminal command-line arguments.
#[derive(Debug, Parser)]
#[command(name = "termstage")]
#[command(about = "Manage termstage backend sessions, browser helpers, and semantic APIs")]
#[command(
    long_about = "Manage termstage backend sessions, browser helpers, and semantic APIs. The CLI \
                  requires an explicit command group."
)]
pub struct CliArgs {
    /// Command group.
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum CliCommand {
    /// Manage backend sessions.
    Session(SessionArgs),
    /// Call semantic API operations on a running gateway.
    Api(ApiArgs),
    /// Manage browser gateway helpers.
    Web(WebArgs),
    /// Inspect local auth state.
    Auth(AuthArgs),
}

#[derive(Debug, Clone, Args)]
struct ServeArgs {
    /// Attach to or create this tmux session.
    #[arg(long, default_value = DEFAULT_SESSION)]
    session: String,
    /// Backend for shared session mode.
    #[arg(long, value_enum)]
    backend: Option<CliBackend>,
    /// Terminal backend mode.
    #[arg(long, value_enum)]
    mode: Option<CliMode>,
    /// Command executable for shell mode.
    #[arg(long)]
    command: Option<PathBuf>,
    /// Argument passed to the shell-mode command. Repeat for multiple arguments.
    #[arg(
        short = 'g',
        long = "command-arg",
        value_name = "ARG",
        allow_hyphen_values = true
    )]
    command_args: Vec<OsString>,
    /// Bind address. Non-loopback addresses require --expose-public.
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,
    /// TCP port. Use 0 for an OS-selected random port.
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Open the tokenized URL in the default browser.
    #[arg(long, default_value_t = false)]
    open: bool,
    /// Browser terminal font size in CSS pixels.
    #[arg(long, default_value_t = DEFAULT_FONT_SIZE)]
    font_size: u16,
    /// Browser terminal theme.
    #[arg(long, value_enum, default_value_t = CliTheme::HighContrast)]
    theme: CliTheme,
    /// Session keepalive policy for browser refresh and shutdown.
    #[arg(long, value_enum, default_value_t = CliKeepalive::Session)]
    keepalive: CliKeepalive,
    /// Child process exit handling policy.
    #[arg(long, value_enum)]
    exit_policy: Option<CliExitPolicy>,
    /// Enable internet-facing pod mode behind an HTTPS ingress.
    #[arg(long, default_value_t = false)]
    expose_public: bool,
    /// Browser-visible HTTPS base URL for public mode.
    #[arg(long)]
    public_url: Option<String>,
    /// Environment variable containing the 64-hex-character access token.
    #[arg(long)]
    token_env: Option<String>,
    /// Reverse-proxy base path under which to mount all routes
    /// (e.g. `/p/<sessionId>/`). Must start and end with `/`.
    #[arg(long)]
    base_path: Option<String>,
}

#[derive(Debug, Clone, Args)]
struct SessionArgs {
    /// Session command.
    #[command(subcommand)]
    command: SessionCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum SessionCommand {
    /// Create a backend session and persist its termstage session id.
    Create {
        /// Backend that owns the real session.
        #[arg(long, value_enum, default_value_t = CliBackend::Tmux)]
        backend: CliBackend,
        /// Human-readable backend session name.
        #[arg(long)]
        name: String,
        /// Command executable for the first backend pane. Defaults to $SHELL.
        #[arg(long)]
        command: Option<PathBuf>,
        /// Argument passed to the backend command. Repeat for multiple args.
        #[arg(
            short = 'g',
            long = "command-arg",
            value_name = "ARG",
            allow_hyphen_values = true
        )]
        command_args: Vec<OsString>,
    },
    /// List sessions visible to termstage.
    List {
        /// Optional backend filter.
        #[arg(long, value_enum)]
        backend: Option<CliBackend>,
    },
    /// Inspect one session.
    Inspect {
        /// Termstage session id.
        session_id: String,
    },
    /// Stop managing a session or kill its backend session.
    Stop {
        /// Termstage session id.
        session_id: String,
        /// Kill the backend session.
        #[arg(long, conflicts_with = "detach")]
        kill: bool,
        /// Keep the backend session and detach only.
        #[arg(long, conflicts_with = "kill")]
        detach: bool,
    },
}

#[derive(Debug, Clone, Args)]
struct ApiArgs {
    /// Running termstage gateway base URL.
    #[arg(long, global = true)]
    url: Option<String>,
    /// Gateway access token.
    #[arg(long, global = true)]
    token: Option<String>,
    /// Agent controller id used for write operations.
    #[arg(long, global = true, default_value_t = 1)]
    controller_id: u64,
    /// Semantic API command.
    #[command(subcommand)]
    command: ApiCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum ApiCommand {
    /// Acquire the write lease for an agent controller.
    AcquireLock {
        /// Session name.
        session: String,
    },
    /// Release the write lease for an agent controller.
    ReleaseLock {
        /// Session name.
        session: String,
    },
    /// Send literal text.
    SendText {
        /// Session name.
        session: String,
        /// Text to send.
        text: String,
    },
    /// Send one key token.
    SendKey {
        /// Session name.
        session: String,
        /// Key token, for example `Enter` or `CtrlC`.
        key: String,
    },
    /// Submit a command and optionally wait/capture.
    RunCommand {
        /// Session name.
        session: String,
        /// Command text.
        command: String,
        /// Visible text to wait for.
        #[arg(long)]
        wait_for: Option<String>,
        /// Wait timeout in milliseconds.
        #[arg(long)]
        wait_timeout_ms: Option<u64>,
        /// Return a screen capture.
        #[arg(long, default_value_t = false)]
        capture: bool,
    },
    /// Read the visible screen.
    ReadScreen {
        /// Session name.
        session: String,
    },
    /// Scroll backend-visible history.
    Scroll {
        /// Session name.
        session: String,
        /// Scroll direction.
        #[arg(value_enum)]
        direction: CliScrollDirection,
        /// Scroll amount.
        amount: u16,
    },
}

#[derive(Debug, Clone, Args)]
struct WebArgs {
    /// Browser gateway helper command.
    #[command(subcommand)]
    command: WebCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum WebCommand {
    /// Attach the browser/API gateway to an existing termstage session id.
    Attach(WebAttachArgs),
    /// Start the browser/API gateway.
    Start(ServeArgs),
    /// Build a tokenized browser URL from a base URL and token.
    Url {
        /// Base URL printed by `termstage web start`.
        #[arg(long)]
        base_url: String,
        /// Gateway access token.
        #[arg(long)]
        token: String,
    },
    /// Token helper commands.
    Token {
        /// Token command.
        #[command(subcommand)]
        command: WebTokenCommand,
    },
}

#[derive(Debug, Clone, Args)]
struct WebAttachArgs {
    /// Termstage session id returned by `termstage session create`.
    session_id: String,
    /// Bind address. Non-loopback addresses require --expose-public.
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,
    /// TCP port. Use 0 for an OS-selected random port.
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Open the tokenized URL in the default browser.
    #[arg(long, default_value_t = false)]
    open: bool,
    /// Browser terminal font size in CSS pixels.
    #[arg(long, default_value_t = DEFAULT_FONT_SIZE)]
    font_size: u16,
    /// Browser terminal theme.
    #[arg(long, value_enum, default_value_t = CliTheme::HighContrast)]
    theme: CliTheme,
    /// Enable internet-facing pod mode behind an HTTPS ingress.
    #[arg(long, default_value_t = false)]
    expose_public: bool,
    /// Browser-visible HTTPS base URL for public mode.
    #[arg(long)]
    public_url: Option<String>,
    /// Environment variable containing the 64-hex-character access token.
    #[arg(long)]
    token_env: Option<String>,
    /// Reverse-proxy base path under which to mount all routes
    /// (e.g. `/p/<sessionId>/`). Must start and end with `/`.
    #[arg(long)]
    base_path: Option<String>,
}

#[derive(Debug, Clone, Subcommand)]
enum WebTokenCommand {
    /// Generate a 64-hex-character access token.
    Generate,
}

#[derive(Debug, Clone, Args)]
struct AuthArgs {
    /// Auth helper command.
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum AuthCommand {
    /// Show local auth mode.
    Status,
}

/// Validated CLI configuration.
#[derive(Debug, Clone)]
pub struct ValidatedCliConfig {
    /// Runtime configuration.
    pub runtime: RuntimeConfig,
    /// Bind host.
    pub host: IpAddr,
    /// TCP bind port.
    pub port: u16,
    /// Whether to open the browser.
    pub open: bool,
    /// Browser presentation settings.
    pub presentation: PresentationSettings,
    /// Browser terminal exposure mode.
    pub exposure: WebExposure,
    /// Access token for this server run.
    pub token: AccessToken,
    /// Optional reverse-proxy base path.
    pub base_path: Option<BasePath>,
}

#[derive(Debug, Clone)]
struct ValidatedWebAttachConfig {
    session_id: SessionName,
    host: IpAddr,
    port: u16,
    open: bool,
    presentation: PresentationSettings,
    exposure: WebExposure,
    token: AccessToken,
    base_path: Option<BasePath>,
}

#[derive(Debug, Clone)]
enum ValidatedCliCommand {
    WebStart(ValidatedCliConfig),
    WebAttach(ValidatedWebAttachConfig),
    Session(SessionArgs),
    Api(ApiArgs),
    Web(WebArgs),
    Auth(AuthArgs),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSessionRegistry {
    version: u16,
    sessions: Vec<StoredSessionRecord>,
}

impl StoredSessionRegistry {
    fn new() -> Self {
        Self {
            version: SESSION_REGISTRY_VERSION,
            sessions: Vec::new(),
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.version != SESSION_REGISTRY_VERSION {
            bail!("unsupported session registry version {}", self.version);
        }
        if self.sessions.len() > SESSION_REGISTRY_MAX_RECORDS {
            bail!("session registry contains too many records");
        }
        for record in &self.sessions {
            record.validate()?;
        }
        Ok(())
    }

    fn insert(&mut self, record: StoredSessionRecord) -> anyhow::Result<()> {
        if self
            .sessions
            .iter()
            .any(|existing| existing.id == record.id)
        {
            bail!("termstage session id {} already exists", record.id);
        }
        if self.sessions.iter().any(|existing| {
            existing.backend == record.backend && existing.backend_session == record.backend_session
        }) {
            bail!(
                "backend session {} already has a termstage session id",
                record.backend_session
            );
        }
        if self.sessions.len() >= SESSION_REGISTRY_MAX_RECORDS {
            bail!("session registry contains too many records");
        }
        self.sessions.push(record);
        Ok(())
    }

    fn find(&self, session_id: &SessionName) -> anyhow::Result<&StoredSessionRecord> {
        self.sessions
            .iter()
            .find(|record| record.id == session_id.as_str())
            .with_context(|| format!("termstage session id {} was not found", session_id.as_str()))
    }

    fn remove(&mut self, session_id: &SessionName) -> anyhow::Result<StoredSessionRecord> {
        let index = self
            .sessions
            .iter()
            .position(|record| record.id == session_id.as_str())
            .with_context(|| {
                format!("termstage session id {} was not found", session_id.as_str())
            })?;
        Ok(self.sessions.remove(index))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSessionRecord {
    id: String,
    backend: CliBackend,
    name: String,
    backend_session: String,
    created_at: String,
    command: Vec<String>,
}

impl StoredSessionRecord {
    fn validate(&self) -> anyhow::Result<()> {
        SessionName::from_str(&self.id).context("invalid registry session id")?;
        SessionName::from_str(&self.backend_session).context("invalid registry backend session")?;
        validate_registry_text(&self.name, SESSION_LABEL_MAX_BYTES, "session name")?;
        validate_registry_text(
            &self.created_at,
            SESSION_CREATED_AT_MAX_BYTES,
            "session created timestamp",
        )?;
        if self.command.len() > SESSION_COMMAND_MAX_ARGS {
            bail!("session command contains too many arguments");
        }
        if self.command.is_empty() {
            bail!("session command must not be empty");
        }
        for value in &self.command {
            validate_registry_text(value, SESSION_COMMAND_ARG_MAX_BYTES, "session command arg")?;
        }
        Ok(())
    }
}

impl TryFrom<CliArgs> for ValidatedCliCommand {
    type Error = anyhow::Error;

    fn try_from(args: CliArgs) -> Result<Self, Self::Error> {
        match args.command {
            CliCommand::Session(command) => Ok(Self::Session(validate_session_args(command)?)),
            CliCommand::Api(command) => Ok(Self::Api(command)),
            CliCommand::Web(command) => match command.command {
                WebCommand::Attach(attach) => {
                    Ok(Self::WebAttach(ValidatedWebAttachConfig::try_from(attach)?))
                }
                WebCommand::Start(serve) => {
                    Ok(Self::WebStart(ValidatedCliConfig::try_from(serve)?))
                }
                command => Ok(Self::Web(WebArgs { command })),
            },
            CliCommand::Auth(command) => Ok(Self::Auth(command)),
        }
    }
}

impl TryFrom<WebAttachArgs> for ValidatedWebAttachConfig {
    type Error = anyhow::Error;

    fn try_from(args: WebAttachArgs) -> Result<Self, Self::Error> {
        if !(MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&args.font_size) {
            bail!("font size must be in {MIN_FONT_SIZE}..={MAX_FONT_SIZE}");
        }
        let base_path = match args.base_path.as_deref() {
            Some(value) => Some(BasePath::from_str(value).context("invalid --base-path")?),
            None => None,
        };
        let (exposure, token) = exposure_and_token_from_parts(
            args.expose_public,
            args.host,
            args.public_url.as_deref(),
            args.token_env.as_deref(),
            |name| env::var(name),
        )?;
        Ok(Self {
            session_id: SessionName::from_str(&args.session_id).context("invalid session id")?,
            host: args.host,
            port: args.port,
            open: args.open,
            presentation: PresentationSettings {
                font_size: args.font_size,
                theme: args.theme.into(),
            },
            exposure,
            token,
            base_path,
        })
    }
}

impl TryFrom<ServeArgs> for ValidatedCliConfig {
    type Error = anyhow::Error;

    fn try_from(args: ServeArgs) -> Result<Self, Self::Error> {
        if !(MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&args.font_size) {
            bail!("font size must be in {MIN_FONT_SIZE}..={MAX_FONT_SIZE}");
        }
        if args.backend.is_some() && args.mode.is_some() {
            bail!("--backend cannot be combined with legacy --mode");
        }
        let base_path = match args.base_path.as_deref() {
            Some(value) => Some(BasePath::from_str(value).context("invalid --base-path")?),
            None => None,
        };
        let (exposure, token) = exposure_and_token(&args)?;
        let initial_size = initial_terminal_size()?;
        let effective_mode = effective_session_mode(args.backend, args.mode)?;
        if args.command.is_some() && !matches!(effective_mode, EffectiveSessionMode::LegacyShell) {
            bail!("--command requires --mode shell");
        }
        if !args.command_args.is_empty()
            && !matches!(effective_mode, EffectiveSessionMode::LegacyShell)
        {
            bail!("--command-arg requires --mode shell");
        }
        let explicit_shell_command = args.command.is_some();
        let mode = match effective_mode {
            EffectiveSessionMode::TmuxBackend => SessionMode::Tmux {
                session: SessionName::from_str(&args.session)
                    .context("invalid tmux session name")?,
            },
            EffectiveSessionMode::LegacyShell => SessionMode::NewShell {
                shell: shell_mode_command(args.command, args.command_args)?,
            },
            EffectiveSessionMode::RmuxBackend => {
                bail!("rmux backend is not implemented in this build");
            }
        };
        let exit_policy = match args.exit_policy {
            Some(policy) => policy.into(),
            None if explicit_shell_command => ExitPolicy::End,
            None => ExitPolicy::Hold,
        };
        Ok(Self {
            runtime: RuntimeConfig {
                mode,
                initial_size,
                reconnect_policy: args.keepalive.into(),
                exit_policy,
            },
            host: args.host,
            port: args.port,
            open: args.open,
            presentation: PresentationSettings {
                font_size: args.font_size,
                theme: args.theme.into(),
            },
            exposure,
            token,
            base_path,
        })
    }
}

/// Runs the CLI application.
///
/// # Errors
///
/// Returns an error when validation, runtime startup, server startup, or
/// graceful shutdown fails.
pub async fn run() -> anyhow::Result<()> {
    init_tracing();
    let args = CliArgs::parse();
    let command = ValidatedCliCommand::try_from(args)?;
    run_validated_command(command).await
}

async fn run_validated_command(command: ValidatedCliCommand) -> anyhow::Result<()> {
    match command {
        ValidatedCliCommand::WebStart(config) => run_with_config(config).await,
        ValidatedCliCommand::WebAttach(config) => run_web_attach(config).await,
        ValidatedCliCommand::Session(args) => run_session_command(args).await,
        ValidatedCliCommand::Api(args) => run_api_command(args).await,
        ValidatedCliCommand::Web(args) => run_web_command(args),
        ValidatedCliCommand::Auth(args) => run_auth_command(args),
    }
}

/// Runs the browser terminal from a validated configuration.
///
/// # Errors
///
/// Returns an error when runtime or server startup fails.
pub async fn run_with_config(config: ValidatedCliConfig) -> anyhow::Result<()> {
    reject_root_user()?;
    match config.runtime.mode.clone() {
        SessionMode::Tmux { session } => run_gateway_tmux(config, session).await,
        SessionMode::NewShell { .. } => run_legacy_runtime(config).await,
    }
}

async fn run_legacy_runtime(config: ValidatedCliConfig) -> anyhow::Result<()> {
    let session = RuntimeSession::start(config.runtime.clone())
        .context("failed to start browser terminal runtime")?;
    let mut web_config = WebConfig::local(config.token, session.command_sender(), config.runtime);
    web_config.host = config.host;
    web_config.port = config.port;
    web_config.presentation = config.presentation;
    web_config.exposure = config.exposure;
    web_config.base_path = config.base_path;

    let server = serve(web_config)
        .await
        .context("failed to start browser terminal server")?;
    let launch_url = server.launch_url();
    if config.open {
        if let Err(error) = open::that_detached(&launch_url) {
            eprintln!("{launch_url}");
            info!(%error, "failed to open browser; printed launch URL");
        }
    } else {
        eprintln!("{launch_url}");
    }
    info!(address = %server.address(), "browser terminal server started");
    let shutdown_reason = wait_for_shutdown_or_runtime_exit(&session).await;
    server
        .shutdown()
        .await
        .context("failed to shutdown browser terminal server")?;
    session
        .shutdown(shutdown_reason)
        .await
        .context("failed to shutdown browser terminal runtime")
}

async fn run_gateway_tmux(config: ValidatedCliConfig, session: SessionName) -> anyhow::Result<()> {
    let backend = TmuxBackend::from_path().context("failed to resolve tmux backend")?;
    let mut gateway = SessionGateway::new(backend, GATEWAY_LEASE_TTL);
    let registration = gateway
        .create_or_find_session(
            session.clone(),
            session.clone(),
            config.runtime.initial_size,
        )
        .await
        .context("failed to create or find tmux backend session")?;
    eprintln!(
        "{}",
        tmux_gateway_session_status(&session, registration.backend_created())
    );
    let gateway = Arc::new(Mutex::new(gateway));
    let mut web_config =
        WebConfig::local_tmux_gateway(config.token, Arc::clone(&gateway), session.clone());
    web_config.host = config.host;
    web_config.port = config.port;
    web_config.presentation = config.presentation;
    web_config.exposure = config.exposure;
    web_config.base_path = config.base_path;

    let server = serve(web_config)
        .await
        .context("failed to start browser terminal server")?;
    let launch_url = server.launch_url();
    if config.open {
        if let Err(error) = open::that_detached(&launch_url) {
            eprintln!("{launch_url}");
            info!(%error, "failed to open browser; printed launch URL");
        }
    } else {
        eprintln!("{launch_url}");
    }
    info!(address = %server.address(), "browser terminal server started");
    wait_for_gateway_shutdown_or_session_end(gateway, session).await;
    server
        .shutdown()
        .await
        .context("failed to shutdown browser terminal server")
}

async fn run_web_attach(config: ValidatedWebAttachConfig) -> anyhow::Result<()> {
    reject_root_user()?;
    let path = session_registry_path()?;
    let registry = load_session_registry(&path).await?;
    let record = registry.find(&config.session_id)?.clone();
    match record.backend {
        CliBackend::Tmux => run_web_attach_tmux(config, record).await,
        CliBackend::Rmux => bail!("rmux backend is not implemented in this build"),
    }
}

async fn run_web_attach_tmux(
    config: ValidatedWebAttachConfig,
    record: StoredSessionRecord,
) -> anyhow::Result<()> {
    let backend_session =
        SessionName::from_str(&record.backend_session).context("invalid backend session")?;
    let backend = TmuxBackend::from_path().context("failed to resolve tmux backend")?;
    let backend_ref = backend
        .attach_existing_session(&backend_session)
        .await
        .with_context(|| format!("failed to attach tmux session {}", backend_session.as_str()))?;
    let mut gateway = SessionGateway::new(backend, GATEWAY_LEASE_TTL);
    gateway
        .register_existing_session(config.session_id.clone(), backend_ref)
        .context("failed to register backend session with gateway")?;
    let gateway = Arc::new(Mutex::new(gateway));
    let mut web_config = WebConfig::local_tmux_gateway(
        config.token,
        Arc::clone(&gateway),
        config.session_id.clone(),
    );
    web_config.host = config.host;
    web_config.port = config.port;
    web_config.presentation = config.presentation;
    web_config.exposure = config.exposure;
    web_config.base_path = config.base_path;

    let server = serve(web_config)
        .await
        .context("failed to start browser terminal server")?;
    let launch_url = server.launch_url();
    if config.open {
        if let Err(error) = open::that_detached(&launch_url) {
            eprintln!("{launch_url}");
            info!(%error, "failed to open browser; printed launch URL");
        }
    } else {
        eprintln!("{launch_url}");
    }
    info!(address = %server.address(), "browser terminal server started");
    wait_for_gateway_shutdown_or_session_end(gateway, config.session_id).await;
    server
        .shutdown()
        .await
        .context("failed to shutdown browser terminal server")
}

fn shell_mode_command(path: Option<PathBuf>, args: Vec<OsString>) -> anyhow::Result<ShellCommand> {
    match path {
        Some(path) => ShellCommand::new(path, args).map_err(Into::into),
        None if args.is_empty() => ShellCommand::default_unix().map_err(Into::into),
        None => {
            let command = ShellCommand::default_unix()?;
            let executable = command.executable().to_path_buf();
            let mut command_args = command.args().to_vec();
            command_args.extend(args);
            ShellCommand::new(executable, command_args).map_err(Into::into)
        }
    }
}

fn validate_session_args(args: SessionArgs) -> anyhow::Result<SessionArgs> {
    match &args.command {
        SessionCommand::Create {
            command,
            command_args,
            ..
        } => {
            if command.is_none() && !command_args.is_empty() {
                bail!("--command-arg requires --command");
            }
        }
        SessionCommand::Stop { kill, detach, .. } => match (*kill, *detach) {
            (true, false) | (false, true) => {}
            (false, false) => bail!("session stop requires exactly one of --detach or --kill"),
            (true, true) => bail!("session stop cannot combine --detach and --kill"),
        },
        SessionCommand::List { .. } | SessionCommand::Inspect { .. } => {}
    }
    Ok(args)
}

fn tmux_gateway_session_status(session: &SessionName, created: bool) -> String {
    let action = if created { "created" } else { "reused" };
    format!(
        "tmux session {} {action}; attach with: tmux attach -t {}",
        session.as_str(),
        session.as_str()
    )
}

fn session_registry_path() -> anyhow::Result<PathBuf> {
    let home = env::var_os("HOME").context("$HOME is not set")?;
    Ok(PathBuf::from(home).join(".termstage").join("sessions.json"))
}

async fn load_session_registry(path: &Path) -> anyhow::Result<StoredSessionRegistry> {
    match fs::metadata(path).await {
        Ok(metadata) => {
            if metadata.len() > SESSION_REGISTRY_MAX_BYTES {
                bail!("session registry exceeds {SESSION_REGISTRY_MAX_BYTES} bytes");
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(StoredSessionRegistry::new());
        }
        Err(error) => return Err(error).context("failed to stat session registry"),
    }
    let text = fs::read_to_string(path)
        .await
        .context("failed to read session registry")?;
    let registry: StoredSessionRegistry =
        serde_json::from_str(&text).context("failed to parse session registry")?;
    registry.validate()?;
    Ok(registry)
}

async fn save_session_registry(
    path: &Path,
    registry: &StoredSessionRegistry,
) -> anyhow::Result<()> {
    registry.validate()?;
    let parent = path
        .parent()
        .context("session registry path must include a parent directory")?;
    fs::create_dir_all(parent)
        .await
        .context("failed to create session registry directory")?;
    let temporary = path.with_extension("json.tmp");
    let text =
        serde_json::to_string_pretty(registry).context("failed to serialize session registry")?;
    fs::write(&temporary, text)
        .await
        .context("failed to write session registry")?;
    set_owner_only_file_permissions(&temporary).await?;
    fs::rename(&temporary, path)
        .await
        .context("failed to replace session registry")?;
    Ok(())
}

async fn set_owner_only_file_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let permissions = std::fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions)
            .await
            .context("failed to set registry permissions")?;
    }
    #[cfg(not(unix))]
    {
        let _path = path;
    }
    Ok(())
}

fn validate_registry_text(value: &str, max_bytes: usize, label: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        bail!("{label} must not be empty");
    }
    if value.len() > max_bytes {
        bail!("{label} must be at most {max_bytes} bytes");
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        bail!("{label} must not contain control characters");
    }
    Ok(())
}

fn generate_session_id() -> anyhow::Result<SessionName> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?;
    let value = format!("ts-{:x}-{:x}", process::id(), elapsed.as_nanos());
    SessionName::from_str(&value).context("generated session id was invalid")
}

fn command_record(command: Option<&PathBuf>, args: &[OsString]) -> anyhow::Result<Vec<String>> {
    let Some(command) = command else {
        return Ok(vec!["$SHELL".to_owned()]);
    };
    let mut values = Vec::with_capacity(args.len().saturating_add(1));
    let command = command
        .to_str()
        .context("--command must be valid UTF-8 to persist session metadata")?;
    validate_registry_text(command, SESSION_COMMAND_ARG_MAX_BYTES, "session command")?;
    values.push(command.to_owned());
    for arg in args {
        let value = arg
            .to_str()
            .context("--command-arg must be valid UTF-8 to persist session metadata")?;
        validate_registry_text(value, SESSION_COMMAND_ARG_MAX_BYTES, "session command arg")?;
        values.push(value.to_owned());
    }
    Ok(values)
}

fn tmux_session_command(
    command: Option<PathBuf>,
    args: Vec<OsString>,
) -> Option<TmuxSessionCommand> {
    command.map(|path| TmuxSessionCommand::new(path.into_os_string(), args))
}

fn initial_terminal_size() -> anyhow::Result<TerminalSize> {
    terminal_size_from_tty()
        .or_else(|_tty_error| terminal_size_from_env())
        .or_else(|_env_error| default_terminal_size())
}

fn default_terminal_size() -> anyhow::Result<TerminalSize> {
    TerminalSize::new(80, 24).context("default terminal size is invalid")
}

fn terminal_size_from_env() -> anyhow::Result<TerminalSize> {
    let cols = env::var("COLUMNS").context("$COLUMNS is not set")?;
    let rows = env::var("LINES").context("$LINES is not set")?;
    terminal_size_from_values(&cols, &rows)
}

fn terminal_size_from_tty() -> anyhow::Result<TerminalSize> {
    #[cfg(unix)]
    {
        let size = rustix::termios::tcgetwinsize(io::stdin())
            .or_else(|_stdin_error| rustix::termios::tcgetwinsize(io::stdout()))
            .or_else(|_stdout_error| rustix::termios::tcgetwinsize(io::stderr()))
            .context("failed to read terminal size from stdio")?;
        TerminalSize::new(size.ws_col, size.ws_row).context("terminal size is invalid")
    }
    #[cfg(not(unix))]
    {
        bail!("terminal size detection from stdio is unsupported on this platform");
    }
}

fn terminal_size_from_values(cols: &str, rows: &str) -> anyhow::Result<TerminalSize> {
    let cols = cols
        .parse::<u16>()
        .with_context(|| format!("invalid terminal columns value {cols:?}"))?;
    let rows = rows
        .parse::<u16>()
        .with_context(|| format!("invalid terminal rows value {rows:?}"))?;
    TerminalSize::new(cols, rows).context("terminal size is invalid")
}

fn exposure_and_token(args: &ServeArgs) -> anyhow::Result<(WebExposure, AccessToken)> {
    exposure_and_token_with_env(args, |name| env::var(name))
}

fn exposure_and_token_with_env(
    args: &ServeArgs,
    get_env: impl Fn(&str) -> Result<String, env::VarError>,
) -> anyhow::Result<(WebExposure, AccessToken)> {
    exposure_and_token_from_parts(
        args.expose_public,
        args.host,
        args.public_url.as_deref(),
        args.token_env.as_deref(),
        get_env,
    )
}

fn exposure_and_token_from_parts(
    expose_public: bool,
    host: IpAddr,
    public_url: Option<&str>,
    token_env: Option<&str>,
    get_env: impl Fn(&str) -> Result<String, env::VarError>,
) -> anyhow::Result<(WebExposure, AccessToken)> {
    if expose_public {
        let public_url = public_url
            .context("--public-url is required with --expose-public")
            .and_then(|value| {
                value
                    .parse::<PublicBaseUrl>()
                    .context("invalid --public-url for public exposure")
            })?;
        let token_env = token_env.context("--token-env is required with --expose-public")?;
        validate_token_env_name(token_env)?;
        let token_value = get_env(token_env)
            .with_context(|| format!("failed to read access token from ${token_env}"))?;
        let token = AccessToken::from_str(&token_value)
            .with_context(|| format!("invalid access token in ${token_env}"))?;
        Ok((WebExposure::Public { public_url }, token))
    } else {
        if !host.is_loopback() {
            bail!("browser terminal bind host must be loopback unless --expose-public is set");
        }
        if public_url.is_some() {
            bail!("--public-url requires --expose-public");
        }
        if token_env.is_some() {
            bail!("--token-env requires --expose-public");
        }
        let token = AccessToken::generate().context("failed to generate browser access token")?;
        Ok((WebExposure::Local, token))
    }
}

fn validate_token_env_name(value: &str) -> anyhow::Result<()> {
    let bytes = value.as_bytes();
    let Some(first) = bytes.first() else {
        bail!("--token-env must not be empty");
    };
    if value.len() > TOKEN_ENV_MAX_BYTES {
        bail!("--token-env must be at most {TOKEN_ENV_MAX_BYTES} bytes");
    }
    if !first.is_ascii_uppercase() && *first != b'_' {
        bail!("--token-env must start with A-Z or _");
    }
    let valid_rest = bytes
        .iter()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_');
    if !valid_rest {
        bail!("--token-env must contain only A-Z, 0-9, and _");
    }
    Ok(())
}

async fn wait_for_shutdown_or_runtime_exit(session: &RuntimeSession) -> ShutdownReason {
    let mut interval = tokio::time::interval(ACTOR_EXIT_POLL_INTERVAL);
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                match result {
                    Ok(()) | Err(_) => {}
                }
                return ShutdownReason::Supervisor;
            }
            _ = interval.tick() => {
                if session.is_finished() {
                    return ShutdownReason::ChildExit;
                }
            }
        }
    }
}

async fn wait_for_gateway_shutdown_or_session_end(
    gateway: Arc<Mutex<SessionGateway<TmuxBackend>>>,
    session: SessionName,
) {
    let mut interval = tokio::time::interval(GATEWAY_EXIT_POLL_INTERVAL);
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                match result {
                    Ok(()) | Err(_) => {}
                }
                return;
            }
            _ = interval.tick() => {
                let result = {
                    let mut gateway = gateway.lock().await;
                    gateway.read_screen(&session).await
                };
                if let Err(error) = result {
                    debug!(%error, session = session.as_str(), "tmux gateway session ended");
                    tokio::time::sleep(GATEWAY_EXIT_POLL_INTERVAL).await;
                    return;
                }
            }
        }
    }
}

async fn run_session_command(args: SessionArgs) -> anyhow::Result<()> {
    match args.command {
        SessionCommand::Create {
            backend,
            name,
            command,
            command_args,
        } => run_session_create(backend, name, command, command_args).await,
        SessionCommand::List { backend } => {
            let path = session_registry_path()?;
            let registry = load_session_registry(&path).await?;
            let mut stdout = io::stdout().lock();
            for record in registry
                .sessions
                .iter()
                .filter(|record| backend.is_none_or(|kind| kind == record.backend))
            {
                writeln!(
                    stdout,
                    "{}\t{}\t{}\t{}",
                    record.id,
                    record.backend.as_str(),
                    record.name,
                    record.backend_session
                )
                .context("failed to write session list")?;
            }
            Ok(())
        }
        SessionCommand::Inspect { session_id } => run_session_inspect(session_id).await,
        SessionCommand::Stop {
            session_id,
            kill,
            detach,
        } => run_session_stop(session_id, kill, detach).await,
    }
}

async fn run_session_create(
    backend: CliBackend,
    name: String,
    command: Option<PathBuf>,
    command_args: Vec<OsString>,
) -> anyhow::Result<()> {
    let backend_session = SessionName::from_str(&name).context("invalid session name")?;
    let session_id = generate_session_id()?;
    let initial_size = initial_terminal_size()?;
    let command_metadata = command_record(command.as_ref(), &command_args)?;
    let command = tmux_session_command(command, command_args);
    let path = session_registry_path()?;
    let mut registry = load_session_registry(&path).await?;
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_secs()
        .to_string();
    registry.insert(StoredSessionRecord {
        id: session_id.as_str().to_owned(),
        backend,
        name,
        backend_session: backend_session.as_str().to_owned(),
        created_at,
        command: command_metadata,
    })?;
    create_backend_session(backend, &backend_session, initial_size, command.as_ref()).await?;
    if let Err(error) = save_session_registry(&path, &registry).await {
        if let Err(rollback_error) = kill_backend_session(backend, &backend_session).await {
            warn!(
                %rollback_error,
                backend = backend.as_str(),
                backend_session = backend_session.as_str(),
                "failed to rollback backend session after registry save failure"
            );
        }
        return Err(error).context("failed to persist created session");
    }

    let mut stdout = io::stdout().lock();
    writeln!(stdout, "id: {}", session_id.as_str()).context("failed to write session create")?;
    writeln!(stdout, "backend: {}", backend.as_str()).context("failed to write session create")?;
    writeln!(stdout, "name: {}", backend_session.as_str())
        .context("failed to write session create")?;
    writeln!(
        stdout,
        "attach: tmux attach -t {}",
        backend_session.as_str()
    )
    .context("failed to write session create")?;
    writeln!(stdout, "web: termstage web attach {}", session_id.as_str())
        .context("failed to write session create")?;
    Ok(())
}

async fn run_session_inspect(session_id: String) -> anyhow::Result<()> {
    let session_id = SessionName::from_str(&session_id).context("invalid session id")?;
    let path = session_registry_path()?;
    let registry = load_session_registry(&path).await?;
    let record = registry.find(&session_id)?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "id: {}", record.id).context("failed to write session details")?;
    writeln!(stdout, "backend: {}", record.backend.as_str())
        .context("failed to write session details")?;
    writeln!(stdout, "name: {}", record.name).context("failed to write session details")?;
    writeln!(stdout, "backend-session: {}", record.backend_session)
        .context("failed to write session details")?;
    writeln!(stdout, "created-at: {}", record.created_at)
        .context("failed to write session details")?;
    writeln!(stdout, "command: {}", record.command.join(" "))
        .context("failed to write session details")?;
    match record.backend {
        CliBackend::Tmux => {
            let backend_session = SessionName::from_str(&record.backend_session)
                .context("invalid backend session")?;
            let backend = TmuxBackend::from_path().context("failed to resolve tmux backend")?;
            let info = backend
                .inspect_session(&backend_session)
                .await
                .with_context(|| {
                    format!(
                        "failed to inspect tmux session {}",
                        backend_session.as_str()
                    )
                })?;
            writeln!(stdout, "window: {}", info.window().as_str())
                .context("failed to write session details")?;
            writeln!(stdout, "pane: {}", info.pane().as_str())
                .context("failed to write session details")?;
            writeln!(
                stdout,
                "size: {}x{}",
                info.size().cols.get(),
                info.size().rows.get()
            )
            .context("failed to write session details")?;
            writeln!(
                stdout,
                "attach: tmux attach -t {}",
                backend_session.as_str()
            )
            .context("failed to write session details")?;
            writeln!(stdout, "web: termstage web attach {}", record.id)
                .context("failed to write session details")?;
        }
        CliBackend::Rmux => bail!("rmux backend is not implemented in this build"),
    }
    Ok(())
}

async fn run_session_stop(session_id: String, kill: bool, detach: bool) -> anyhow::Result<()> {
    let session_id = SessionName::from_str(&session_id).context("invalid session id")?;
    let path = session_registry_path()?;
    let mut registry = load_session_registry(&path).await?;
    let record = registry.remove(&session_id)?;
    if kill {
        match record.backend {
            CliBackend::Tmux => {
                let backend_session = SessionName::from_str(&record.backend_session)
                    .context("invalid backend session")?;
                if kill_backend_session(CliBackend::Tmux, &backend_session).await? {
                    writeln!(
                        io::stdout(),
                        "killed tmux session {} for termstage session {}",
                        backend_session.as_str(),
                        session_id.as_str()
                    )
                    .context("failed to write stop result")?;
                } else {
                    writeln!(
                        io::stdout(),
                        "removed stale termstage session {}; backend session {} was already gone",
                        session_id.as_str(),
                        backend_session.as_str()
                    )
                    .context("failed to write stop result")?;
                }
            }
            CliBackend::Rmux => bail!("rmux backend is not implemented in this build"),
        }
    } else if detach {
        writeln!(
            io::stdout(),
            "detached termstage session {}; backend session is still available with: {}",
            session_id.as_str(),
            native_attach_command(&record)
        )
        .context("failed to write stop result")?;
    }
    save_session_registry(&path, &registry).await?;
    Ok(())
}

async fn create_backend_session(
    backend: CliBackend,
    backend_session: &SessionName,
    initial_size: TerminalSize,
    command: Option<&TmuxSessionCommand>,
) -> anyhow::Result<()> {
    match backend {
        CliBackend::Tmux => {
            let tmux = TmuxBackend::from_path().context("failed to resolve tmux backend")?;
            tmux.create_new_session(backend_session, initial_size, command)
                .await
                .with_context(|| {
                    format!("failed to create tmux session {}", backend_session.as_str())
                })?;
            Ok(())
        }
        CliBackend::Rmux => bail!("rmux backend is not implemented in this build"),
    }
}

async fn kill_backend_session(
    backend: CliBackend,
    backend_session: &SessionName,
) -> anyhow::Result<bool> {
    match backend {
        CliBackend::Tmux => {
            let tmux = TmuxBackend::from_path().context("failed to resolve tmux backend")?;
            if !tmux
                .session_exists_by_name(backend_session)
                .await
                .with_context(|| {
                    format!("failed to check tmux session {}", backend_session.as_str())
                })?
            {
                return Ok(false);
            }
            tmux.kill_session_by_name(backend_session)
                .await
                .with_context(|| {
                    format!("failed to kill tmux session {}", backend_session.as_str())
                })?;
            Ok(true)
        }
        CliBackend::Rmux => bail!("rmux backend is not implemented in this build"),
    }
}

fn native_attach_command(record: &StoredSessionRecord) -> String {
    match record.backend {
        CliBackend::Tmux => format!("tmux attach -t {}", record.backend_session),
        CliBackend::Rmux => format!("rmux attach -t {}", record.backend_session),
    }
}

async fn run_api_command(args: ApiArgs) -> anyhow::Result<()> {
    let token = args.token.as_deref().context("--token is required")?;
    let token = AccessToken::from_str(token).context("invalid --token")?;
    let (session, endpoint, body) = api_request(args)?;
    let url = api_endpoint_url(&endpoint.url, &session, endpoint.path, &token)?;
    let response = post_json(&url, &body).await?;
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(response.as_bytes())
        .context("failed to write API response")?;
    if !response.ends_with('\n') {
        stdout
            .write_all(b"\n")
            .context("failed to write API response newline")?;
    }
    Ok(())
}

#[derive(Debug)]
struct ApiEndpoint {
    url: String,
    path: &'static str,
}

fn api_request(args: ApiArgs) -> anyhow::Result<(SessionName, ApiEndpoint, Value)> {
    let url = args.url.context("--url is required")?;
    let controller_id = args.controller_id;
    let request = match args.command {
        ApiCommand::AcquireLock { session } => (
            SessionName::from_str(&session).context("invalid session name")?,
            "acquire-lock",
            json!({ "controllerId": controller_id }),
        ),
        ApiCommand::ReleaseLock { session } => (
            SessionName::from_str(&session).context("invalid session name")?,
            "release-lock",
            json!({ "controllerId": controller_id }),
        ),
        ApiCommand::SendText { session, text } => (
            SessionName::from_str(&session).context("invalid session name")?,
            "write-text",
            json!({ "controllerId": controller_id, "text": text }),
        ),
        ApiCommand::SendKey { session, key } => (
            SessionName::from_str(&session).context("invalid session name")?,
            "press-key",
            json!({ "controllerId": controller_id, "key": key }),
        ),
        ApiCommand::RunCommand {
            session,
            command,
            wait_for,
            wait_timeout_ms,
            capture,
        } => (
            SessionName::from_str(&session).context("invalid session name")?,
            "run-command",
            json!({
                "controllerId": controller_id,
                "command": command,
                "waitFor": wait_for,
                "waitTimeoutMs": wait_timeout_ms,
                "capture": capture,
            }),
        ),
        ApiCommand::ReadScreen { session } => (
            SessionName::from_str(&session).context("invalid session name")?,
            "read-screen",
            Value::Null,
        ),
        ApiCommand::Scroll {
            session,
            direction,
            amount,
        } => (
            SessionName::from_str(&session).context("invalid session name")?,
            "scroll",
            json!({
                "controllerId": controller_id,
                "direction": direction.as_json(),
                "amount": amount,
            }),
        ),
    };
    Ok((
        request.0,
        ApiEndpoint {
            url,
            path: request.1,
        },
        request.2,
    ))
}

fn run_web_command(args: WebArgs) -> anyhow::Result<()> {
    match args.command {
        WebCommand::Attach(_attach) => bail!("web attach must be dispatched before web helpers"),
        WebCommand::Start(_serve) => bail!("web start must be dispatched before web helpers"),
        WebCommand::Url { base_url, token } => {
            let token = AccessToken::from_str(&token).context("invalid --token")?;
            let url = browser_url(&base_url, &token)?;
            writeln!(io::stdout(), "{url}").context("failed to write browser URL")?;
            Ok(())
        }
        WebCommand::Token { command } => match command {
            WebTokenCommand::Generate => {
                let token = AccessToken::generate().context("failed to generate access token")?;
                writeln!(io::stdout(), "{}", token.to_url_token())
                    .context("failed to write access token")?;
                Ok(())
            }
        },
    }
}

fn run_auth_command(args: AuthArgs) -> anyhow::Result<()> {
    let AuthArgs { command } = args;
    match command {
        AuthCommand::Status => {
            writeln!(
                io::stdout(),
                "auth: local token mode; OIDC is not configured in this build"
            )
            .context("failed to write auth status")?;
            Ok(())
        }
    }
}

fn api_endpoint_url(
    base_url: &str,
    session: &SessionName,
    endpoint: &str,
    token: &AccessToken,
) -> anyhow::Result<Url> {
    let mut url = parse_http_url(base_url)?;
    let prefix = url.path().trim_end_matches('/');
    let path = if prefix.is_empty() {
        format!("/api/sessions/{}/{endpoint}", session.as_str())
    } else {
        format!("{prefix}/api/sessions/{}/{endpoint}", session.as_str())
    };
    url.set_path(&path);
    url.set_query(None);
    url.query_pairs_mut()
        .append_pair("token", &token.to_url_token());
    Ok(url)
}

fn browser_url(base_url: &str, token: &AccessToken) -> anyhow::Result<Url> {
    let mut url = Url::parse(base_url).context("invalid --base-url")?;
    if url.fragment().is_some() {
        bail!("--base-url must not contain a fragment");
    }
    url.query_pairs_mut()
        .append_pair("token", &token.to_url_token());
    Ok(url)
}

fn parse_http_url(value: &str) -> anyhow::Result<Url> {
    let url = Url::parse(value).context("invalid --url")?;
    if url.scheme() != "http" {
        bail!("semantic API CLI currently supports http URLs only");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("--url must not contain credentials");
    }
    if url.host().is_none() {
        bail!("--url must include a host");
    }
    if url.fragment().is_some() {
        bail!("--url must not contain a fragment");
    }
    Ok(url)
}

async fn post_json(url: &Url, body: &Value) -> anyhow::Result<String> {
    let host = url.host().context("API URL missing host")?;
    let port = url
        .port_or_known_default()
        .context("API URL missing port")?;
    let connect_host = connect_host(&host);
    let mut stream = TcpStream::connect((connect_host.as_str(), port))
        .await
        .with_context(|| format!("failed to connect to {connect_host}:{port}"))?;
    let body = if body.is_null() {
        String::new()
    } else {
        serde_json::to_string(body).context("failed to serialize API request")?
    };
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: \
         {}\r\nConnection: close\r\n\r\n{}",
        request_target(url),
        host_header(&host, port),
        body.len(),
        body,
    );
    stream
        .write_all(request.as_bytes())
        .await
        .context("failed to write API request")?;
    let response = read_capped_response(stream).await?;
    let parsed = parse_http_response(&response)?;
    if (200..300).contains(&parsed.status) {
        String::from_utf8(parsed.body).context("API response body was not valid UTF-8")
    } else {
        let body = match String::from_utf8(parsed.body) {
            Ok(body) => body,
            Err(error) => format!("{:?}", error.as_bytes()),
        };
        bail!("API request failed with HTTP {}: {body}", parsed.status);
    }
}

#[derive(Debug)]
struct ParsedHttpResponse {
    status: u16,
    body: Vec<u8>,
}

async fn read_capped_response(mut stream: TcpStream) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let bytes = stream
            .read(&mut chunk)
            .await
            .context("failed to read API response")?;
        if bytes == 0 {
            return Ok(output);
        }
        let next_len = output
            .len()
            .checked_add(bytes)
            .context("API response length overflow")?;
        if next_len > API_RESPONSE_MAX_BYTES {
            bail!("API response exceeded {API_RESPONSE_MAX_BYTES} bytes");
        }
        let Some(slice) = chunk.get(..bytes) else {
            bail!("API response chunk length was invalid");
        };
        output.extend_from_slice(slice);
    }
}

fn parse_http_response(response: &[u8]) -> anyhow::Result<ParsedHttpResponse> {
    let header_end = response
        .windows(HTTP_HEADER_SEPARATOR.len())
        .position(|window| window == HTTP_HEADER_SEPARATOR)
        .context("API response did not contain HTTP headers")?;
    let (header_bytes, body_bytes) = response.split_at(header_end);
    let body_bytes = body_bytes
        .get(HTTP_HEADER_SEPARATOR.len()..)
        .context("API response body offset was invalid")?;
    let headers = std::str::from_utf8(header_bytes).context("API response headers were invalid")?;
    let status = parse_status(headers)?;
    let body = if has_chunked_transfer(headers) {
        decode_chunked_body(body_bytes)?
    } else {
        body_bytes.to_vec()
    };
    Ok(ParsedHttpResponse { status, body })
}

fn parse_status(headers: &str) -> anyhow::Result<u16> {
    let status_line = headers
        .lines()
        .next()
        .context("API response missing status line")?;
    let mut parts = status_line.split_ascii_whitespace();
    let version = parts.next().context("API response missing HTTP version")?;
    if !version.starts_with("HTTP/") {
        bail!("API response had invalid HTTP version");
    }
    let status = parts
        .next()
        .context("API response missing HTTP status")?
        .parse::<u16>()
        .context("API response status was invalid")?;
    Ok(status)
}

fn has_chunked_transfer(headers: &str) -> bool {
    headers.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("transfer-encoding")
                && value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("chunked"))
        })
    })
}

fn decode_chunked_body(mut body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let line_end = body
            .windows(HTTP_LINE_SEPARATOR.len())
            .position(|window| window == HTTP_LINE_SEPARATOR)
            .context("chunked response missing chunk size")?;
        let (size_bytes, rest) = body.split_at(line_end);
        let rest = rest
            .get(HTTP_LINE_SEPARATOR.len()..)
            .context("chunked response size offset was invalid")?;
        let size_line =
            std::str::from_utf8(size_bytes).context("chunk size was not valid UTF-8")?;
        let size_hex = size_line
            .split(';')
            .next()
            .context("chunk size was missing")?
            .trim();
        let size =
            usize::from_str_radix(size_hex, 16).context("chunk size was not valid hexadecimal")?;
        if size == 0 {
            return Ok(output);
        }
        let chunk_end = size
            .checked_add(HTTP_LINE_SEPARATOR.len())
            .context("chunk length overflow")?;
        if rest.len() < chunk_end {
            bail!("chunked response ended before chunk body");
        }
        let chunk = rest
            .get(..size)
            .context("chunked response chunk offset was invalid")?;
        output.extend_from_slice(chunk);
        let separator = rest
            .get(size..chunk_end)
            .context("chunked response separator offset was invalid")?;
        if separator != HTTP_LINE_SEPARATOR {
            bail!("chunked response chunk was not terminated");
        }
        body = rest
            .get(chunk_end..)
            .context("chunked response next offset was invalid")?;
    }
}

fn request_target(url: &Url) -> String {
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    }
}

fn connect_host(host: &Host<&str>) -> String {
    match host {
        Host::Domain(value) => (*value).to_owned(),
        Host::Ipv4(value) => value.to_string(),
        Host::Ipv6(value) => value.to_string(),
    }
}

fn host_header(host: &Host<&str>, port: u16) -> String {
    match host {
        Host::Domain(value) => format!("{value}:{port}"),
        Host::Ipv4(value) => format!("{value}:{port}"),
        Host::Ipv6(value) => format!("[{value}]:{port}"),
    }
}

fn init_tracing() {
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_error| tracing_subscriber::EnvFilter::new("termstage=info")),
        )
        .finish();
    let _result = tracing::subscriber::set_global_default(subscriber);
}

fn reject_root_user() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        if rustix::process::geteuid().is_root() {
            bail!("browser terminal must not run as root");
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "camelCase")]
enum CliBackend {
    Tmux,
    Rmux,
}

impl CliBackend {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Rmux => "rmux",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliMode {
    Tmux,
    Shell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectiveSessionMode {
    TmuxBackend,
    RmuxBackend,
    LegacyShell,
}

fn effective_session_mode(
    backend: Option<CliBackend>,
    mode: Option<CliMode>,
) -> anyhow::Result<EffectiveSessionMode> {
    match (backend, mode) {
        (Some(CliBackend::Tmux), None) | (None, Some(CliMode::Tmux) | None) => {
            Ok(EffectiveSessionMode::TmuxBackend)
        }
        (Some(CliBackend::Rmux), None) => Ok(EffectiveSessionMode::RmuxBackend),
        (None, Some(CliMode::Shell)) => Ok(EffectiveSessionMode::LegacyShell),
        (Some(_), Some(_)) => bail!("--backend cannot be combined with legacy --mode"),
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliScrollDirection {
    Up,
    Down,
}

impl CliScrollDirection {
    fn as_json(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliTheme {
    HighContrast,
    Light,
}

impl From<CliTheme> for PresentationTheme {
    fn from(value: CliTheme) -> Self {
        match value {
            CliTheme::HighContrast => Self::HighContrast,
            CliTheme::Light => Self::Light,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliKeepalive {
    Session,
    Exit,
}

impl From<CliKeepalive> for ReconnectPolicy {
    fn from(value: CliKeepalive) -> Self {
        match value {
            CliKeepalive::Session => Self::KeepAlive,
            CliKeepalive::Exit => Self::TerminateOnShutdown,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliExitPolicy {
    Hold,
    End,
}

impl From<CliExitPolicy> for ExitPolicy {
    fn from(value: CliExitPolicy) -> Self {
        match value {
            CliExitPolicy::Hold => Self::Hold,
            CliExitPolicy::End => Self::End,
        }
    }
}

impl Default for ServeArgs {
    fn default() -> Self {
        Self {
            session: DEFAULT_SESSION.to_owned(),
            backend: None,
            mode: None,
            command: None,
            command_args: Vec::new(),
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            open: false,
            font_size: DEFAULT_FONT_SIZE,
            theme: CliTheme::HighContrast,
            keepalive: CliKeepalive::Session,
            exit_policy: None,
            expose_public: false,
            public_url: None,
            token_env: None,
            base_path: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_validate_default_cli_config() -> anyhow::Result<()> {
        let config = ValidatedCliConfig::try_from(ServeArgs::default())?;
        assert!(config.host.is_loopback());
        assert_eq!(config.port, 0);
        assert_eq!(config.presentation.font_size, DEFAULT_FONT_SIZE);
        assert!(matches!(config.exposure, WebExposure::Local));
        assert!(matches!(config.runtime.mode, SessionMode::Tmux { .. }));
        assert_eq!(config.runtime.exit_policy, ExitPolicy::Hold);
        Ok(())
    }

    #[test]
    fn test_should_parse_initial_terminal_size_values() -> anyhow::Result<()> {
        assert_eq!(
            terminal_size_from_values("132", "43")?,
            TerminalSize::new(132, 43)?
        );
        Ok(())
    }

    #[test]
    fn test_should_reject_invalid_initial_terminal_size_values() {
        assert!(terminal_size_from_values("0", "24").is_err());
        assert!(terminal_size_from_values("80", "0").is_err());
        assert!(terminal_size_from_values("wide", "24").is_err());
    }

    #[test]
    fn test_should_reject_non_loopback_host() {
        let args = ServeArgs {
            host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            ..ServeArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_validate_public_exposure_config() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([3; 32]).to_url_token();
        let args = ServeArgs {
            host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: 8080,
            expose_public: true,
            public_url: Some("https://term.example.com/".to_owned()),
            token_env: Some("TERMSTAGE_TOKEN".to_owned()),
            ..ServeArgs::default()
        };
        let (exposure, parsed_token) = exposure_and_token_with_env(&args, |name| {
            if name == "TERMSTAGE_TOKEN" {
                Ok(token.clone())
            } else {
                Err(env::VarError::NotPresent)
            }
        })?;
        assert!(matches!(exposure, WebExposure::Public { .. }));
        assert!(parsed_token.constant_time_eq(&AccessToken::from_bytes([3; 32])));
        Ok(())
    }

    #[test]
    fn test_should_reject_public_mode_without_required_flags() {
        let args = ServeArgs {
            expose_public: true,
            ..ServeArgs::default()
        };
        assert!(
            exposure_and_token_with_env(&args, |_name| Err(env::VarError::NotPresent)).is_err()
        );
    }

    #[test]
    fn test_should_reject_public_args_without_public_mode() {
        let args = ServeArgs {
            public_url: Some("https://term.example.com/".to_owned()),
            ..ServeArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_reject_invalid_token_env_name() {
        assert!(validate_token_env_name("TERMSTAGE_TOKEN").is_ok());
        assert!(validate_token_env_name("termstage_token").is_err());
        assert!(validate_token_env_name("").is_err());
        assert!(validate_token_env_name("TERMSTAGE-TOKEN").is_err());
    }

    #[test]
    fn test_should_reject_invalid_session_name() {
        let args = ServeArgs {
            session: "bad/name".to_owned(),
            ..ServeArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_validate_shell_mode_without_session_use() -> anyhow::Result<()> {
        let args = ServeArgs {
            mode: Some(CliMode::Shell),
            command: Some(PathBuf::from("/bin/sh")),
            ..ServeArgs::default()
        };
        let config = ValidatedCliConfig::try_from(args)?;
        assert!(matches!(config.runtime.mode, SessionMode::NewShell { .. }));
        assert_eq!(config.runtime.exit_policy, ExitPolicy::End);
        Ok(())
    }

    #[test]
    fn test_should_allow_explicit_shell_command_hold_policy() -> anyhow::Result<()> {
        let args = ServeArgs {
            mode: Some(CliMode::Shell),
            command: Some(PathBuf::from("/bin/sh")),
            exit_policy: Some(CliExitPolicy::Hold),
            ..ServeArgs::default()
        };
        let config = ValidatedCliConfig::try_from(args)?;
        assert_eq!(config.runtime.exit_policy, ExitPolicy::Hold);
        Ok(())
    }

    #[test]
    fn test_should_pass_shell_arguments_to_shell_mode() -> anyhow::Result<()> {
        let args = ServeArgs {
            mode: Some(CliMode::Shell),
            command: Some(PathBuf::from("codemax")),
            command_args: vec![OsString::from("claude")],
            ..ServeArgs::default()
        };
        let config = ValidatedCliConfig::try_from(args)?;
        let shell = match config.runtime.mode {
            SessionMode::NewShell { shell } => shell,
            SessionMode::Tmux { .. } => {
                anyhow::bail!("validated shell mode must produce a new shell command");
            }
        };
        assert_eq!(shell.executable(), PathBuf::from("codemax").as_path());
        assert_eq!(shell.args(), [OsString::from("claude")]);
        Ok(())
    }

    #[test]
    fn test_should_reject_command_args_outside_shell_mode() {
        let args = ServeArgs {
            command_args: vec![OsString::from("claude")],
            ..ServeArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_reject_command_outside_shell_mode() {
        let args = ServeArgs {
            command: Some(PathBuf::from("claude")),
            ..ServeArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_parse_short_command_args() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from([
            "termstage",
            "web",
            "start",
            "--mode",
            "shell",
            "--command",
            "abc",
            "-g=-p",
            "-g=--resume",
        ])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::WebStart(config) = command else {
            anyhow::bail!("web start command must validate to web start config");
        };
        let shell = match config.runtime.mode {
            SessionMode::NewShell { shell } => shell,
            SessionMode::Tmux { .. } => {
                anyhow::bail!("validated shell mode must produce a new shell command");
            }
        };
        assert_eq!(shell.executable(), PathBuf::from("abc").as_path());
        assert_eq!(
            shell.args(),
            [OsString::from("-p"), OsString::from("--resume")]
        );
        Ok(())
    }

    #[test]
    fn test_should_parse_web_start_command() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from([
            "termstage",
            "web",
            "start",
            "--backend",
            "tmux",
            "--session",
            "demo",
        ])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::WebStart(config) = command else {
            anyhow::bail!("web start command must validate to web start config");
        };
        let SessionMode::Tmux { session } = config.runtime.mode else {
            anyhow::bail!("tmux backend must validate to tmux runtime mode");
        };
        assert_eq!(session.as_str(), "demo");
        Ok(())
    }

    #[test]
    fn test_should_reject_backend_and_legacy_mode_together() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from([
            "termstage",
            "web",
            "start",
            "--backend",
            "tmux",
            "--mode",
            "shell",
        ])?;
        assert!(ValidatedCliCommand::try_from(args).is_err());
        Ok(())
    }

    #[test]
    fn test_should_require_top_level_subcommand() {
        let result = CliArgs::try_parse_from(["termstage"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_should_reject_removed_root_serve_alias() {
        let result = CliArgs::try_parse_from(["termstage", "--session", "demo"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_should_parse_session_command_group() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from(["termstage", "session", "list"])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::List { backend: None }
        ));
        Ok(())
    }

    #[test]
    fn test_should_parse_session_create_command() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from([
            "termstage",
            "session",
            "create",
            "--backend",
            "tmux",
            "--name",
            "demo",
            "--command",
            "k9s",
            "-g=--readonly",
        ])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::Create {
                backend: CliBackend::Tmux,
                ref name,
                ref command,
                ref command_args,
            } if name == "demo"
                && command.as_ref().is_some_and(|path| path == &PathBuf::from("k9s"))
                && command_args == &[OsString::from("--readonly")]
        ));
        Ok(())
    }

    #[test]
    fn test_should_reject_session_create_args_without_command() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from([
            "termstage",
            "session",
            "create",
            "--name",
            "demo",
            "-g=--readonly",
        ])?;
        assert!(ValidatedCliCommand::try_from(args).is_err());
        Ok(())
    }

    #[test]
    fn test_should_record_default_session_command_as_shell() -> anyhow::Result<()> {
        assert_eq!(command_record(None, &[])?, ["$SHELL".to_owned()]);
        Ok(())
    }

    #[test]
    fn test_should_parse_web_attach_command() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from(["termstage", "web", "attach", "ts-1-2", "--open"])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::WebAttach(config) = command else {
            anyhow::bail!("web attach command must validate to web attach config");
        };
        assert_eq!(config.session_id.as_str(), "ts-1-2");
        assert!(config.open);
        Ok(())
    }

    #[tokio::test]
    async fn test_should_round_trip_session_registry() -> anyhow::Result<()> {
        let root = env::temp_dir().join(format!(
            "termstage-registry-test-{}-{}",
            process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir_all(&root).await?;
        let path = root.join("sessions.json");
        let mut registry = StoredSessionRegistry::new();
        registry.insert(StoredSessionRecord {
            id: "ts-1-2".to_owned(),
            backend: CliBackend::Tmux,
            name: "demo".to_owned(),
            backend_session: "demo".to_owned(),
            created_at: "1".to_owned(),
            command: vec!["k9s".to_owned(), "--readonly".to_owned()],
        })?;

        save_session_registry(&path, &registry).await?;
        let loaded = load_session_registry(&path).await?;

        assert_eq!(loaded.find(&SessionName::from_str("ts-1-2")?)?.name, "demo");
        fs::remove_file(path).await?;
        fs::remove_dir(root).await?;
        Ok(())
    }

    #[test]
    fn test_should_require_session_stop_action() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from(["termstage", "session", "stop", "demo"])?;
        assert!(ValidatedCliCommand::try_from(args).is_err());
        Ok(())
    }

    #[test]
    fn test_should_reject_conflicting_session_stop_actions() {
        let result =
            CliArgs::try_parse_from(["termstage", "session", "stop", "demo", "--detach", "--kill"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_should_accept_session_stop_detach_action() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from(["termstage", "session", "stop", "demo", "--detach"])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::Stop {
                kill: false,
                detach: true,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn test_should_report_tmux_gateway_session_status() -> anyhow::Result<()> {
        let session = SessionName::from_str("demo")?;

        assert_eq!(
            tmux_gateway_session_status(&session, true),
            "tmux session demo created; attach with: tmux attach -t demo"
        );
        assert_eq!(
            tmux_gateway_session_status(&session, false),
            "tmux session demo reused; attach with: tmux attach -t demo"
        );
        Ok(())
    }

    #[test]
    fn test_should_parse_api_command_group_with_global_options() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([9; 32]).to_url_token();
        let args = CliArgs::try_parse_from([
            "termstage",
            "api",
            "read-screen",
            "demo",
            "--url",
            "http://127.0.0.1:1234",
            "--token",
            &token,
        ])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Api(args) = command else {
            anyhow::bail!("api command group must validate to api args");
        };
        assert_eq!(args.url.as_deref(), Some("http://127.0.0.1:1234"));
        assert_eq!(args.token.as_deref(), Some(token.as_str()));
        assert!(matches!(
            args.command,
            ApiCommand::ReadScreen { ref session } if session == "demo"
        ));
        Ok(())
    }

    #[test]
    fn test_should_parse_web_and_auth_command_groups() -> anyhow::Result<()> {
        let web = CliArgs::try_parse_from(["termstage", "web", "token", "generate"])?;
        assert!(matches!(
            ValidatedCliCommand::try_from(web)?,
            ValidatedCliCommand::Web(WebArgs {
                command: WebCommand::Token {
                    command: WebTokenCommand::Generate,
                },
            })
        ));

        let auth = CliArgs::try_parse_from(["termstage", "auth", "status"])?;
        assert!(matches!(
            ValidatedCliCommand::try_from(auth)?,
            ValidatedCliCommand::Auth(AuthArgs {
                command: AuthCommand::Status,
            })
        ));
        Ok(())
    }

    #[test]
    fn test_should_build_api_endpoint_url_with_base_path() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([7; 32]);
        let session = SessionName::from_str("demo")?;
        let url = api_endpoint_url(
            "http://127.0.0.1:1234/p/demo/",
            &session,
            "read-screen",
            &token,
        )?;
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:1234/p/demo/api/sessions/demo/read-screen?token=0707070707070707070707070707070707070707070707070707070707070707"
        );
        Ok(())
    }

    #[test]
    fn test_should_decode_chunked_http_response() -> anyhow::Result<()> {
        let response =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let parsed = parse_http_response(response)?;
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.body, b"hello");
        Ok(())
    }

    #[test]
    fn test_should_reject_removed_local_attach_short_flag() {
        let result = CliArgs::try_parse_from([
            "termstage",
            "web",
            "start",
            "--mode",
            "shell",
            "--command",
            "abc",
            "-a",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_should_reject_invalid_font_size() {
        let args = ServeArgs {
            font_size: MAX_FONT_SIZE.saturating_add(1),
            ..ServeArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_validate_base_path_argument() -> anyhow::Result<()> {
        let args = ServeArgs {
            base_path: Some("/p/sess-1/".to_owned()),
            ..ServeArgs::default()
        };
        let config = ValidatedCliConfig::try_from(args)?;
        assert_eq!(
            config.base_path.as_ref().map(BasePath::as_str),
            Some("/p/sess-1/")
        );
        Ok(())
    }

    #[test]
    fn test_should_reject_invalid_base_path_argument() {
        let args = ServeArgs {
            base_path: Some("p/missing-leading-slash/".to_owned()),
            ..ServeArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_accept_non_root_test_process() -> anyhow::Result<()> {
        if !rustix::process::geteuid().is_root() {
            reject_root_user()?;
        }
        Ok(())
    }
}
