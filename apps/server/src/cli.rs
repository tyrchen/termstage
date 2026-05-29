//! Command-line interface for browser terminal mode.

use std::{
    env,
    ffi::OsString,
    io::{self, Write},
    net::IpAddr,
    path::PathBuf,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use termstage_core::{
    backend::{BackendAdapter, BackendScreenSnapshot, BackendScrollDirection, BackendSessionRef},
    protocol::{AccessToken, SessionName, TerminalSize},
    rmux_backend::{RmuxBackend, RmuxSessionCommand},
    security::{BasePath, PublicBaseUrl},
    session_gateway::SessionGateway,
    tmux_backend::{TmuxBackend, TmuxSessionCommand},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    process::Command,
    sync::Mutex,
    time,
};
use tracing::{debug, info};
use url::{Host, Url};

use crate::{
    settings::{
        api_client, backend_gateway, browser_presentation, cli_defaults, semantic_api,
        session_names,
    },
    web::{BackendGateway, PresentationSettings, PresentationTheme, WebConfig, WebExposure, serve},
};

/// Browser terminal command-line arguments.
#[derive(Debug, Parser)]
#[command(name = "termstage")]
#[command(about = "Manage termstage backend sessions, browser gateway, and semantic APIs")]
#[command(
    long_about = "Manage termstage backend sessions, browser gateway, and semantic APIs. The CLI \
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
    /// Inspect local auth state.
    Auth(AuthArgs),
}

#[derive(Debug, Clone, Args)]
struct SessionArgs {
    /// Session command.
    #[command(subcommand)]
    command: SessionCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum SessionCommand {
    /// Create a backend session and print its termstage session id.
    Create {
        /// Backend that owns the real session.
        #[arg(long, value_enum, default_value_t = CliBackend::Tmux)]
        backend: CliBackend,
        /// Human-readable session name. Termstage-created sessions are named
        /// `TerminalUse-<name>`.
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
    /// Attach to one session. Native attach is default; use --browser for browser-mode attach.
    Attach(SessionAttachArgs),
    /// List sessions visible to termstage.
    List {
        /// Optional backend filter.
        #[arg(long, value_enum)]
        backend: Option<CliBackend>,
    },
    /// Inspect one session.
    Inspect {
        /// Termstage session id or backend session name.
        session_id: String,
    },
    /// Read the visible screen directly from the backend session.
    Screen {
        /// Termstage session id or backend session name.
        session_id: String,
        /// Optional backend filter when the session id is ambiguous.
        #[arg(long, value_enum)]
        backend: Option<CliBackend>,
    },
    /// Send literal text directly to the backend session.
    SendText {
        /// Termstage session id or backend session name.
        session_id: String,
        /// Optional backend filter when the session id is ambiguous.
        #[arg(long, value_enum)]
        backend: Option<CliBackend>,
        /// Text to send.
        text: String,
    },
    /// Send one key token directly to the backend session.
    SendKey {
        /// Termstage session id or backend session name.
        session_id: String,
        /// Optional backend filter when the session id is ambiguous.
        #[arg(long, value_enum)]
        backend: Option<CliBackend>,
        /// Key token, for example `Enter` or `CtrlC`.
        key: String,
    },
    /// Type a command, press Enter, and optionally wait/capture.
    #[command(visible_alias = "run-command")]
    Exec {
        /// Termstage session id or backend session name.
        session_id: String,
        /// Optional backend filter when the session id is ambiguous.
        #[arg(long, value_enum)]
        backend: Option<CliBackend>,
        /// Visible text to wait for.
        #[arg(long)]
        wait_for: Option<String>,
        /// Wait timeout in milliseconds.
        #[arg(long)]
        wait_timeout_ms: Option<u64>,
        /// Return a screen capture.
        #[arg(long, default_value_t = false)]
        capture: bool,
        /// Command argv to type into the session. Use `--` before the command.
        #[arg(last = true, required = true, num_args = 1..)]
        command: Vec<String>,
    },
    /// Scroll backend-visible history directly on the backend session.
    Scroll {
        /// Termstage session id or backend session name.
        session_id: String,
        /// Optional backend filter when the session id is ambiguous.
        #[arg(long, value_enum)]
        backend: Option<CliBackend>,
        /// Scroll direction.
        #[arg(value_enum)]
        direction: CliScrollDirection,
        /// Scroll amount.
        amount: u16,
    },
    /// Stop a session by killing its backend session.
    Stop {
        /// Termstage session id or backend session name.
        session_id: String,
    },
}

#[derive(Debug, Clone, Args)]
struct SessionAttachArgs {
    /// Termstage session id or backend session name.
    session_id: String,
    /// Optional backend filter when the session id is ambiguous.
    #[arg(long, value_enum)]
    backend: Option<CliBackend>,
    /// Start a browser/API gateway instead of native backend attach.
    #[arg(long, visible_alias = "broswer", default_value_t = false)]
    browser: bool,
    /// Bind address used with --browser. Non-loopback addresses require --expose-public.
    #[arg(long, default_value = "127.0.0.1", requires = "browser")]
    host: IpAddr,
    /// TCP port used with --browser. Use 0 for an OS-selected random port.
    #[arg(long, default_value_t = 0, requires = "browser")]
    port: u16,
    /// Open the tokenized URL in the default browser when used with --browser.
    #[arg(long, default_value_t = false, requires = "browser")]
    open: bool,
    /// Browser terminal font size in CSS pixels.
    #[arg(
        long,
        default_value_t = browser_presentation::DEFAULT_FONT_SIZE,
        requires = "browser"
    )]
    font_size: u16,
    /// Browser terminal theme.
    #[arg(
        long,
        value_enum,
        default_value_t = CliTheme::HighContrast,
        requires = "browser"
    )]
    theme: CliTheme,
    /// Enable internet-facing pod mode behind an HTTPS ingress.
    #[arg(long, default_value_t = false, requires = "browser")]
    expose_public: bool,
    /// Browser-visible HTTPS base URL for public mode.
    #[arg(long, requires = "browser")]
    public_url: Option<String>,
    /// Environment variable containing the 64-hex-character access token.
    #[arg(long, requires = "browser")]
    token_env: Option<String>,
    /// Reverse-proxy base path under which to mount all routes
    /// (e.g. `/p/<sessionId>/`). Must start and end with `/`.
    #[arg(long, requires = "browser")]
    base_path: Option<String>,
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

#[derive(Debug, Clone)]
struct ValidatedSessionAttachConfig {
    session_id: SessionName,
    backend: Option<CliBackend>,
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
    Session(SessionArgs),
    Api(ApiArgs),
    Auth(AuthArgs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliSessionRecord {
    id: String,
    backend: CliBackend,
    display_name: String,
    backend_session: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliSessionInspectDetails {
    record: CliSessionRecord,
    window: String,
    pane: String,
    size: TerminalSize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedCliSession {
    Tmux {
        record: CliSessionRecord,
        backend_session: SessionName,
    },
    Rmux {
        record: CliSessionRecord,
        backend_session: SessionName,
    },
}

#[derive(Debug, Clone)]
enum BackendSessionCommand {
    Tmux(Option<TmuxSessionCommand>),
    Rmux(Option<RmuxSessionCommand>),
}

impl TryFrom<CliArgs> for ValidatedCliCommand {
    type Error = anyhow::Error;

    fn try_from(args: CliArgs) -> Result<Self, Self::Error> {
        match args.command {
            CliCommand::Session(command) => Ok(Self::Session(validate_session_args(command)?)),
            CliCommand::Api(command) => Ok(Self::Api(command)),
            CliCommand::Auth(command) => Ok(Self::Auth(command)),
        }
    }
}

impl TryFrom<SessionAttachArgs> for ValidatedSessionAttachConfig {
    type Error = anyhow::Error;

    fn try_from(args: SessionAttachArgs) -> Result<Self, Self::Error> {
        if !(browser_presentation::FONT_SIZE_MIN..=browser_presentation::FONT_SIZE_MAX)
            .contains(&args.font_size)
        {
            bail!(
                "font size must be in {}..={}",
                browser_presentation::FONT_SIZE_MIN,
                browser_presentation::FONT_SIZE_MAX
            );
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
            backend: args.backend,
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
        ValidatedCliCommand::Session(args) => run_session_command(args).await,
        ValidatedCliCommand::Api(args) => run_api_command(args).await,
        ValidatedCliCommand::Auth(args) => run_auth_command(args),
    }
}

async fn run_session_browser_attach(config: ValidatedSessionAttachConfig) -> anyhow::Result<()> {
    reject_root_user()?;
    match resolve_cli_session(config.backend, &config.session_id).await? {
        ResolvedCliSession::Tmux {
            backend_session, ..
        } => run_session_browser_attach_tmux(config, backend_session).await,
        ResolvedCliSession::Rmux {
            backend_session, ..
        } => run_session_browser_attach_rmux(config, backend_session).await,
    }
}

async fn run_session_browser_attach_tmux(
    config: ValidatedSessionAttachConfig,
    backend_session: SessionName,
) -> anyhow::Result<()> {
    let backend = TmuxBackend::from_path().context("failed to resolve tmux backend")?;
    let backend_ref = backend
        .attach_existing_session(&backend_session)
        .await
        .with_context(|| format!("failed to attach tmux session {}", backend_session.as_str()))?;
    let mut gateway = SessionGateway::new(backend, backend_gateway::OPERATION_LOCK_LEASE_TTL);
    gateway
        .register_existing_session(config.session_id.clone(), backend_ref)
        .context("failed to register backend session with gateway")?;
    let gateway = Arc::new(Mutex::new(gateway));
    let backend_gateway = BackendGateway::Tmux(Arc::clone(&gateway));
    let mut web_config = WebConfig::local_backend_gateway(
        config.token,
        backend_gateway.clone(),
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
    wait_for_gateway_shutdown_or_session_end(backend_gateway, config.session_id).await;
    server
        .shutdown()
        .await
        .context("failed to shutdown browser terminal server")
}

async fn run_session_browser_attach_rmux(
    config: ValidatedSessionAttachConfig,
    backend_session: SessionName,
) -> anyhow::Result<()> {
    let backend = RmuxBackend::connect()
        .await
        .context("failed to connect to rmux backend")?;
    let backend_ref = backend
        .attach_existing_session(&backend_session)
        .await
        .with_context(|| format!("failed to attach rmux session {}", backend_session.as_str()))?;
    let mut gateway = SessionGateway::new(backend, backend_gateway::OPERATION_LOCK_LEASE_TTL);
    gateway
        .register_existing_session(config.session_id.clone(), backend_ref)
        .context("failed to register backend session with gateway")?;
    let gateway = Arc::new(Mutex::new(gateway));
    let backend_gateway = BackendGateway::Rmux(Arc::clone(&gateway));
    let mut web_config = WebConfig::local_backend_gateway(
        config.token,
        backend_gateway.clone(),
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
    wait_for_gateway_shutdown_or_session_end(backend_gateway, config.session_id).await;
    server
        .shutdown()
        .await
        .context("failed to shutdown browser terminal server")
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
        SessionCommand::Attach(_attach) => {}
        SessionCommand::List { .. }
        | SessionCommand::Inspect { .. }
        | SessionCommand::Screen { .. }
        | SessionCommand::SendText { .. }
        | SessionCommand::SendKey { .. }
        | SessionCommand::Exec { .. }
        | SessionCommand::Scroll { .. }
        | SessionCommand::Stop { .. } => {}
    }
    Ok(args)
}

fn tmux_session_command(
    command: Option<PathBuf>,
    args: Vec<OsString>,
) -> Option<TmuxSessionCommand> {
    command.map(|path| TmuxSessionCommand::new(path.into_os_string(), args))
}

fn rmux_session_command(
    command: Option<PathBuf>,
    args: Vec<OsString>,
) -> anyhow::Result<Option<RmuxSessionCommand>> {
    match command {
        Some(path) => Ok(Some(
            RmuxSessionCommand::try_new(path.into_os_string(), args)
                .context("invalid rmux command argv")?,
        )),
        None => Ok(None),
    }
}

fn termstage_tmux_session_name(name: &str) -> anyhow::Result<SessionName> {
    termstage_session_name(name)
}

fn termstage_rmux_session_name(name: &str) -> anyhow::Result<SessionName> {
    termstage_session_name(name)
}

fn termstage_session_name(name: &str) -> anyhow::Result<SessionName> {
    let name = SessionName::from_str(name).context("invalid session name")?;
    if name
        .as_str()
        .starts_with(session_names::TERMSTAGE_SESSION_PREFIX)
    {
        return Ok(name);
    }
    SessionName::from_str(&format!(
        "{}{}",
        session_names::TERMSTAGE_SESSION_PREFIX,
        name.as_str()
    ))
    .context("invalid prefixed session name")
}

fn legacy_termstage_tmux_session_name(name: &str) -> anyhow::Result<SessionName> {
    let name = SessionName::from_str(name).context("invalid session name")?;
    if name
        .as_str()
        .starts_with(session_names::LEGACY_TERMSTAGE_TMUX_SESSION_PREFIX)
    {
        return Ok(name);
    }
    SessionName::from_str(&format!(
        "{}{}",
        session_names::LEGACY_TERMSTAGE_TMUX_SESSION_PREFIX,
        name.as_str()
    ))
    .context("invalid legacy prefixed tmux session name")
}

async fn resolve_tmux_session(
    backend: &TmuxBackend,
    requested: &SessionName,
) -> anyhow::Result<SessionName> {
    if backend
        .session_exists_by_name(requested)
        .await
        .with_context(|| format!("failed to check tmux session {}", requested.as_str()))?
    {
        return Ok(requested.clone());
    }
    let prefixed = termstage_tmux_session_name(requested.as_str())?;
    if prefixed != *requested
        && backend
            .session_exists_by_name(&prefixed)
            .await
            .with_context(|| format!("failed to check tmux session {}", prefixed.as_str()))?
    {
        return Ok(prefixed);
    }
    let legacy_prefixed = legacy_termstage_tmux_session_name(requested.as_str())?;
    if legacy_prefixed != *requested
        && legacy_prefixed != prefixed
        && backend
            .session_exists_by_name(&legacy_prefixed)
            .await
            .with_context(|| format!("failed to check tmux session {}", legacy_prefixed.as_str()))?
    {
        return Ok(legacy_prefixed);
    }
    bail!(
        "tmux session {} was not found; also tried {} and {}",
        requested.as_str(),
        prefixed.as_str(),
        legacy_prefixed.as_str()
    )
}

async fn resolve_rmux_session(
    backend: &RmuxBackend,
    requested: &SessionName,
) -> anyhow::Result<SessionName> {
    if backend
        .session_exists_by_name(requested)
        .await
        .with_context(|| format!("failed to check rmux session {}", requested.as_str()))?
    {
        return Ok(requested.clone());
    }
    let prefixed = termstage_rmux_session_name(requested.as_str())?;
    if prefixed != *requested
        && backend
            .session_exists_by_name(&prefixed)
            .await
            .with_context(|| format!("failed to check rmux session {}", prefixed.as_str()))?
    {
        return Ok(prefixed);
    }
    bail!(
        "rmux session {} was not found; also tried {}",
        requested.as_str(),
        prefixed.as_str()
    )
}

fn cli_record_from_tmux_session(session: &SessionName) -> CliSessionRecord {
    let backend_session = session.as_str().to_owned();
    let display_name = session
        .as_str()
        .strip_prefix(session_names::TERMSTAGE_SESSION_PREFIX)
        .or_else(|| {
            session
                .as_str()
                .strip_prefix(session_names::LEGACY_TERMSTAGE_TMUX_SESSION_PREFIX)
        })
        .unwrap_or(session.as_str())
        .to_owned();
    CliSessionRecord {
        id: backend_session.clone(),
        backend: CliBackend::Tmux,
        display_name,
        backend_session,
    }
}

fn cli_record_from_rmux_session(session: &SessionName) -> CliSessionRecord {
    let backend_session = session.as_str().to_owned();
    let display_name = session
        .as_str()
        .strip_prefix(session_names::TERMSTAGE_SESSION_PREFIX)
        .unwrap_or(session.as_str())
        .to_owned();
    CliSessionRecord {
        id: backend_session.clone(),
        backend: CliBackend::Rmux,
        display_name,
        backend_session,
    }
}

fn cli_record_from_backend_session(backend: CliBackend, session: &SessionName) -> CliSessionRecord {
    match backend {
        CliBackend::Tmux => cli_record_from_tmux_session(session),
        CliBackend::Rmux => cli_record_from_rmux_session(session),
    }
}

async fn resolve_cli_session(
    backend: Option<CliBackend>,
    requested: &SessionName,
) -> anyhow::Result<ResolvedCliSession> {
    match backend {
        Some(CliBackend::Tmux) => resolve_cli_tmux_session(requested).await,
        Some(CliBackend::Rmux) => resolve_cli_rmux_session(requested).await,
        None => resolve_cli_session_auto(requested).await,
    }
}

async fn resolve_cli_session_auto(requested: &SessionName) -> anyhow::Result<ResolvedCliSession> {
    let mut matches = Vec::with_capacity(2);
    if let Ok(session) = resolve_cli_tmux_session(requested).await {
        matches.push(session);
    }
    if let Ok(session) = resolve_cli_rmux_session(requested).await {
        matches.push(session);
    }
    match matches.len() {
        0 => bail!(
            "session {} was not found in tmux or rmux",
            requested.as_str()
        ),
        1 => {
            let Some(session) = matches.pop() else {
                bail!("session resolution failed");
            };
            Ok(session)
        }
        _ => bail!(
            "session {} is ambiguous across backends; pass --backend tmux or --backend rmux",
            requested.as_str()
        ),
    }
}

async fn resolve_cli_tmux_session(requested: &SessionName) -> anyhow::Result<ResolvedCliSession> {
    let backend = TmuxBackend::from_path().context("failed to resolve tmux backend")?;
    let backend_session = resolve_tmux_session(&backend, requested).await?;
    let record = cli_record_from_tmux_session(&backend_session);
    Ok(ResolvedCliSession::Tmux {
        record,
        backend_session,
    })
}

async fn resolve_cli_rmux_session(requested: &SessionName) -> anyhow::Result<ResolvedCliSession> {
    let backend = RmuxBackend::connect()
        .await
        .context("failed to connect to rmux backend")?;
    let backend_session = resolve_rmux_session(&backend, requested).await?;
    let record = cli_record_from_rmux_session(&backend_session);
    Ok(ResolvedCliSession::Rmux {
        record,
        backend_session,
    })
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
    if value.len() > cli_defaults::TOKEN_ENV_MAX_BYTES {
        bail!(
            "--token-env must be at most {} bytes",
            cli_defaults::TOKEN_ENV_MAX_BYTES
        );
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

async fn wait_for_gateway_shutdown_or_session_end(gateway: BackendGateway, session: SessionName) {
    let mut interval = tokio::time::interval(backend_gateway::SESSION_EXIT_POLL_INTERVAL);
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                match result {
                    Ok(()) | Err(_) => {}
                }
                return;
            }
            _ = interval.tick() => {
                let result = gateway.read_screen(&session).await;
                if let Err(error) = result {
                    debug!(%error, session = session.as_str(), "backend gateway session ended");
                    tokio::time::sleep(backend_gateway::SESSION_EXIT_POLL_INTERVAL).await;
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
        SessionCommand::Attach(args) => run_session_attach(args).await,
        SessionCommand::List { backend } => {
            let mut stdout = io::stdout().lock();
            let records = list_cli_sessions(backend).await?;
            write_session_list(&mut stdout, &records)
        }
        SessionCommand::Inspect { session_id } => run_session_inspect(session_id).await,
        SessionCommand::Screen {
            session_id,
            backend,
        } => run_session_screen(session_id, backend).await,
        SessionCommand::SendText {
            session_id,
            backend,
            text,
        } => run_session_send_text(session_id, backend, text).await,
        SessionCommand::SendKey {
            session_id,
            backend,
            key,
        } => run_session_send_key(session_id, backend, key).await,
        SessionCommand::Exec {
            session_id,
            backend,
            command,
            wait_for,
            wait_timeout_ms,
            capture,
        } => {
            run_session_exec(
                session_id,
                backend,
                command,
                wait_for,
                wait_timeout_ms,
                capture,
            )
            .await
        }
        SessionCommand::Scroll {
            session_id,
            backend,
            direction,
            amount,
        } => run_session_scroll(session_id, backend, direction, amount).await,
        SessionCommand::Stop { session_id } => run_session_stop(session_id).await,
    }
}

async fn list_cli_sessions(backend: Option<CliBackend>) -> anyhow::Result<Vec<CliSessionRecord>> {
    match backend {
        None => {
            let mut records: Vec<CliSessionRecord> = list_tmux_sessions().await.unwrap_or_default();
            if let Ok(mut rmux) = list_rmux_sessions().await {
                records.append(&mut rmux);
            }
            Ok(records)
        }
        Some(CliBackend::Tmux) => list_tmux_sessions().await,
        Some(CliBackend::Rmux) => list_rmux_sessions().await,
    }
}

async fn list_tmux_sessions() -> anyhow::Result<Vec<CliSessionRecord>> {
    let tmux = TmuxBackend::from_path().context("failed to resolve tmux backend")?;
    let sessions = tmux
        .list_sessions()
        .await
        .context("failed to list tmux sessions")?;
    Ok(sessions.iter().map(cli_record_from_tmux_session).collect())
}

async fn list_rmux_sessions() -> anyhow::Result<Vec<CliSessionRecord>> {
    let rmux = RmuxBackend::connect()
        .await
        .context("failed to connect to rmux backend")?;
    let sessions = rmux
        .list_sessions()
        .await
        .context("failed to list rmux sessions")?;
    Ok(sessions.iter().map(cli_record_from_rmux_session).collect())
}

fn write_session_list<W: Write>(
    output: &mut W,
    records: &[CliSessionRecord],
) -> anyhow::Result<()> {
    let widths = session_list_widths(records);
    write_session_list_row(output, cli_defaults::SESSION_LIST_HEADERS, widths)?;
    for record in records {
        write_session_list_row(
            output,
            [
                record.id.as_str(),
                record.backend.as_str(),
                record.display_name.as_str(),
            ],
            widths,
        )?;
    }
    Ok(())
}

fn session_list_widths(records: &[CliSessionRecord]) -> [usize; 3] {
    let [mut id_width, mut backend_width, mut display_name_width] =
        cli_defaults::SESSION_LIST_HEADERS.map(str::len);
    for record in records {
        id_width = id_width.max(record.id.len());
        backend_width = backend_width.max(record.backend.as_str().len());
        display_name_width = display_name_width.max(record.display_name.len());
    }
    [id_width, backend_width, display_name_width]
}

fn write_session_list_row<W: Write>(
    output: &mut W,
    values: [&str; 3],
    widths: [usize; 3],
) -> anyhow::Result<()> {
    let [id, backend, display_name] = values;
    let [id_width, backend_width, _display_name_width] = widths;
    writeln!(
        output,
        "{id:<id_width$}  {backend:<backend_width$}  {display_name}"
    )
    .context("failed to write session list")
}

async fn run_session_create(
    backend: CliBackend,
    name: String,
    command: Option<PathBuf>,
    command_args: Vec<OsString>,
) -> anyhow::Result<()> {
    let backend_session = match backend {
        CliBackend::Tmux => termstage_tmux_session_name(&name)?,
        CliBackend::Rmux => termstage_rmux_session_name(&name)?,
    };
    let initial_size = initial_terminal_size()?;
    let command = match backend {
        CliBackend::Tmux => {
            BackendSessionCommand::Tmux(tmux_session_command(command, command_args))
        }
        CliBackend::Rmux => {
            BackendSessionCommand::Rmux(rmux_session_command(command, command_args)?)
        }
    };
    create_backend_session(backend, &backend_session, initial_size, &command).await?;

    let mut stdout = io::stdout().lock();
    writeln!(stdout, "id: {}", backend_session.as_str())
        .context("failed to write session create")?;
    writeln!(stdout, "backend: {}", backend.as_str()).context("failed to write session create")?;
    let record = cli_record_from_backend_session(backend, &backend_session);
    writeln!(stdout, "display-name: {}", record.display_name)
        .context("failed to write session create")?;
    writeln!(stdout, "backend-session: {}", backend_session.as_str())
        .context("failed to write session create")?;
    writeln!(stdout, "attach: {}", native_attach_command(&record))
        .context("failed to write session create")?;
    writeln!(
        stdout,
        "browser: termstage session attach {} --backend {} --browser",
        backend_session.as_str(),
        backend.as_str()
    )
    .context("failed to write session create")?;
    Ok(())
}

async fn run_session_attach(args: SessionAttachArgs) -> anyhow::Result<()> {
    if args.browser {
        let config = ValidatedSessionAttachConfig::try_from(args)?;
        run_session_browser_attach(config).await
    } else {
        let session_id = SessionName::from_str(&args.session_id).context("invalid session id")?;
        run_native_session_attach(args.backend, &session_id).await
    }
}

async fn run_native_session_attach(
    backend: Option<CliBackend>,
    session_id: &SessionName,
) -> anyhow::Result<()> {
    reject_root_user()?;
    let resolved = resolve_cli_session(backend, session_id).await?;
    let record = match resolved {
        ResolvedCliSession::Tmux { record, .. } | ResolvedCliSession::Rmux { record, .. } => record,
    };
    let mut command = native_attach_process(&record);
    let status = command
        .status()
        .await
        .with_context(|| format!("failed to run {}", native_attach_command(&record)))?;
    if !status.success() {
        bail!(
            "{} native attach exited with status {status}",
            record.backend.as_str()
        );
    }
    Ok(())
}

async fn run_session_inspect(session_id: String) -> anyhow::Result<()> {
    let session_id = SessionName::from_str(&session_id).context("invalid session id")?;
    let resolved = resolve_cli_session(None, &session_id).await?;
    let mut stdout = io::stdout().lock();
    match resolved {
        ResolvedCliSession::Tmux {
            record,
            backend_session,
        } => {
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
            let details = CliSessionInspectDetails {
                record,
                window: info.window().as_str().to_owned(),
                pane: info.pane().as_str().to_owned(),
                size: info.size(),
            };
            write_session_inspect_details(&mut stdout, &details)?;
        }
        ResolvedCliSession::Rmux {
            record,
            backend_session,
        } => {
            let backend = RmuxBackend::connect()
                .await
                .context("failed to connect to rmux backend")?;
            let info = backend
                .inspect_session(&backend_session)
                .await
                .with_context(|| {
                    format!(
                        "failed to inspect rmux session {}",
                        backend_session.as_str()
                    )
                })?;
            let details = CliSessionInspectDetails {
                record,
                window: info.window().as_str().to_owned(),
                pane: info.pane().as_str().to_owned(),
                size: info.size(),
            };
            write_session_inspect_details(&mut stdout, &details)?;
        }
    }
    Ok(())
}

async fn run_session_screen(session_id: String, backend: Option<CliBackend>) -> anyhow::Result<()> {
    let session_id = SessionName::from_str(&session_id).context("invalid session id")?;
    let (mut backend, reference) = resolve_local_backend_session(backend, &session_id).await?;
    let snapshot = backend.read_screen(&reference).await?;
    write_json_line(&screen_json(&snapshot))
}

async fn run_session_send_text(
    session_id: String,
    backend: Option<CliBackend>,
    text: String,
) -> anyhow::Result<()> {
    validate_local_text(&text, semantic_api::TEXT_MAX_BYTES, "text")?;
    let session_id = SessionName::from_str(&session_id).context("invalid session id")?;
    let (mut backend, reference) = resolve_local_backend_session(backend, &session_id).await?;
    backend.send_text(&reference, &text).await?;
    write_ok_json()
}

async fn run_session_send_key(
    session_id: String,
    backend: Option<CliBackend>,
    key: String,
) -> anyhow::Result<()> {
    let key = local_semantic_key_token(&key)?;
    let session_id = SessionName::from_str(&session_id).context("invalid session id")?;
    let (mut backend, reference) = resolve_local_backend_session(backend, &session_id).await?;
    backend.send_key(&reference, &key).await?;
    write_ok_json()
}

async fn run_session_exec(
    session_id: String,
    backend: Option<CliBackend>,
    command: Vec<String>,
    wait_for: Option<String>,
    wait_timeout_ms: Option<u64>,
    capture: bool,
) -> anyhow::Result<()> {
    let command = local_exec_command(&command)?;
    let wait_for = wait_for
        .map(|text| validate_wait_text(text, "wait-for"))
        .transpose()?;
    let wait_timeout = semantic_wait_timeout(wait_timeout_ms)?;
    let session_id = SessionName::from_str(&session_id).context("invalid session id")?;
    let (mut backend, reference) = resolve_local_backend_session(backend, &session_id).await?;
    backend.run_command(&reference, &command).await?;
    let mut matched = None;
    let mut screen = None;
    if let Some(wait_for) = wait_for.as_deref() {
        let result =
            wait_for_local_screen_text(&mut backend, &reference, wait_for, wait_timeout).await?;
        matched = Some(result.matched);
        if capture {
            screen = Some(screen_json(&result.snapshot));
        }
    } else if capture {
        let snapshot = backend.read_screen(&reference).await?;
        screen = Some(screen_json(&snapshot));
    }
    let mut response = json!({
        "ok": true,
        "matched": matched,
        "screen": screen,
    });
    if matched.is_none()
        && let Value::Object(object) = &mut response
    {
        object.remove("matched");
    }
    if screen.is_none()
        && let Value::Object(object) = &mut response
    {
        object.remove("screen");
    }
    write_json_line(&response)
}

async fn run_session_scroll(
    session_id: String,
    backend: Option<CliBackend>,
    direction: CliScrollDirection,
    amount: u16,
) -> anyhow::Result<()> {
    let amount = validate_local_scroll_amount(amount)?;
    let session_id = SessionName::from_str(&session_id).context("invalid session id")?;
    let direction = direction.as_backend();
    let (mut backend, reference) = resolve_local_backend_session(backend, &session_id).await?;
    backend.scroll(&reference, direction, amount).await?;
    write_ok_json()
}

async fn resolve_local_backend_session(
    backend: Option<CliBackend>,
    session_id: &SessionName,
) -> anyhow::Result<(LocalBackend, BackendSessionRef)> {
    match resolve_cli_session(backend, session_id).await? {
        ResolvedCliSession::Tmux {
            backend_session, ..
        } => {
            let backend = TmuxBackend::from_path().context("failed to resolve tmux backend")?;
            let backend_ref = backend
                .attach_existing_session(&backend_session)
                .await
                .with_context(|| {
                    format!("failed to attach tmux session {}", backend_session.as_str())
                })?;
            Ok((LocalBackend::Tmux(backend), backend_ref))
        }
        ResolvedCliSession::Rmux {
            backend_session, ..
        } => {
            let backend = RmuxBackend::connect()
                .await
                .context("failed to connect to rmux backend")?;
            let backend_ref = backend
                .attach_existing_session(&backend_session)
                .await
                .with_context(|| {
                    format!("failed to attach rmux session {}", backend_session.as_str())
                })?;
            Ok((LocalBackend::Rmux(backend), backend_ref))
        }
    }
}

#[derive(Debug)]
enum LocalBackend {
    Tmux(TmuxBackend),
    Rmux(RmuxBackend),
}

impl LocalBackend {
    async fn read_screen(
        &mut self,
        reference: &BackendSessionRef,
    ) -> Result<BackendScreenSnapshot, termstage_core::backend::BackendError> {
        match self {
            Self::Tmux(backend) => backend.read_screen(reference).await,
            Self::Rmux(backend) => backend.read_screen(reference).await,
        }
    }

    async fn send_text(
        &mut self,
        reference: &BackendSessionRef,
        text: &str,
    ) -> Result<(), termstage_core::backend::BackendError> {
        match self {
            Self::Tmux(backend) => backend.send_text(reference, text).await,
            Self::Rmux(backend) => backend.send_text(reference, text).await,
        }
    }

    async fn send_key(
        &mut self,
        reference: &BackendSessionRef,
        key: &str,
    ) -> Result<(), termstage_core::backend::BackendError> {
        match self {
            Self::Tmux(backend) => backend.send_key(reference, key).await,
            Self::Rmux(backend) => backend.send_key(reference, key).await,
        }
    }

    async fn run_command(
        &mut self,
        reference: &BackendSessionRef,
        command: &str,
    ) -> Result<(), termstage_core::backend::BackendError> {
        match self {
            Self::Tmux(backend) => backend.run_command(reference, command).await,
            Self::Rmux(backend) => backend.run_command(reference, command).await,
        }
    }

    async fn scroll(
        &mut self,
        reference: &BackendSessionRef,
        direction: BackendScrollDirection,
        amount: u16,
    ) -> Result<(), termstage_core::backend::BackendError> {
        match self {
            Self::Tmux(backend) => backend.scroll(reference, direction, amount).await,
            Self::Rmux(backend) => backend.scroll(reference, direction, amount).await,
        }
    }
}

#[derive(Debug)]
struct LocalWaitResult {
    matched: bool,
    snapshot: BackendScreenSnapshot,
}

async fn wait_for_local_screen_text(
    backend: &mut LocalBackend,
    reference: &BackendSessionRef,
    needle: &str,
    timeout: Duration,
) -> anyhow::Result<LocalWaitResult> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("wait deadline overflow")?;
    loop {
        let snapshot = backend.read_screen(reference).await?;
        if snapshot.lines().iter().any(|line| line.contains(needle)) {
            return Ok(LocalWaitResult {
                matched: true,
                snapshot,
            });
        }
        if Instant::now() >= deadline {
            return Ok(LocalWaitResult {
                matched: false,
                snapshot,
            });
        }
        time::sleep(semantic_api::WAIT_POLL_INTERVAL).await;
    }
}

fn screen_json(snapshot: &BackendScreenSnapshot) -> Value {
    json!({
        "size": snapshot.size(),
        "cursorCol": snapshot.cursor_col(),
        "cursorRow": snapshot.cursor_row(),
        "cursorVisible": snapshot.cursor_visible(),
        "lines": snapshot.lines(),
    })
}

fn write_ok_json() -> anyhow::Result<()> {
    write_json_line(&json!({ "ok": true }))
}

fn write_json_line(value: &Value) -> anyhow::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value).context("failed to write JSON response")?;
    stdout
        .write_all(b"\n")
        .context("failed to write JSON response newline")
}

fn local_exec_command(command: &[String]) -> anyhow::Result<String> {
    let command = command.join(" ");
    validate_local_text(&command, semantic_api::TEXT_MAX_BYTES, "command")?;
    if command.contains(['\r', '\n']) {
        bail!("command must not contain line breaks");
    }
    Ok(command)
}

fn validate_local_text(text: &str, max_bytes: usize, field: &str) -> anyhow::Result<()> {
    if text.len() > max_bytes {
        bail!("{field} must be at most {max_bytes} bytes");
    }
    if text.as_bytes().contains(&0) {
        bail!("{field} must not contain NUL bytes");
    }
    Ok(())
}

fn validate_wait_text(text: String, field: &str) -> anyhow::Result<String> {
    if text.is_empty() {
        bail!("{field} must not be empty");
    }
    validate_local_text(&text, semantic_api::WAIT_TEXT_MAX_BYTES, field)?;
    Ok(text)
}

fn local_semantic_key_token(key: &str) -> anyhow::Result<String> {
    if key.is_empty() || key.len() > semantic_api::KEY_MAX_BYTES {
        bail!(
            "key must be non-empty and at most {} bytes",
            semantic_api::KEY_MAX_BYTES
        );
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
                bail!("unsupported semantic key token");
            }
            key
        }
    };
    Ok(token.to_owned())
}

fn validate_local_scroll_amount(amount: u16) -> anyhow::Result<u16> {
    if amount == 0 || amount > semantic_api::SCROLL_MAX_AMOUNT {
        bail!("amount must be in 1..={}", semantic_api::SCROLL_MAX_AMOUNT);
    }
    Ok(amount)
}

fn semantic_wait_timeout(value: Option<u64>) -> anyhow::Result<Duration> {
    let millis = value.unwrap_or(semantic_api::WAIT_TIMEOUT_DEFAULT_MS);
    if millis == 0 || millis > semantic_api::WAIT_TIMEOUT_MAX_MS {
        bail!(
            "wait timeout must be in 1..={} milliseconds",
            semantic_api::WAIT_TIMEOUT_MAX_MS
        );
    }
    Ok(Duration::from_millis(millis))
}

async fn run_session_stop(session_id: String) -> anyhow::Result<()> {
    let session_id = SessionName::from_str(&session_id).context("invalid session id")?;
    let resolved = resolve_cli_session(None, &session_id).await?;
    let (record, backend_session) = match &resolved {
        ResolvedCliSession::Tmux {
            record,
            backend_session,
        }
        | ResolvedCliSession::Rmux {
            record,
            backend_session,
        } => (record, backend_session),
    };
    if kill_backend_session(record.backend, backend_session).await? {
        writeln!(
            io::stdout(),
            "killed {} session {} for requested session {}",
            record.backend.as_str(),
            backend_session.as_str(),
            session_id.as_str()
        )
        .context("failed to write stop result")?;
    } else {
        writeln!(
            io::stdout(),
            "requested session {} resolved to {}; backend session was already gone",
            session_id.as_str(),
            backend_session.as_str()
        )
        .context("failed to write stop result")?;
    }
    Ok(())
}

fn write_session_inspect_details<W: Write>(
    output: &mut W,
    details: &CliSessionInspectDetails,
) -> anyhow::Result<()> {
    let record = &details.record;
    writeln!(output, "Session:").context("failed to write session details")?;
    write_session_detail_row(output, "id", &record.id)?;
    write_session_detail_row(output, "backend", record.backend.as_str())?;
    write_session_detail_row(output, "display-name", &record.display_name)?;
    writeln!(output).context("failed to write session details")?;
    writeln!(output, "Properties:").context("failed to write session details")?;
    write_session_detail_row(output, "window", &details.window)?;
    write_session_detail_row(output, "pane", &details.pane)?;
    write_session_detail_row(
        output,
        "size",
        &format!("{}x{}", details.size.cols.get(), details.size.rows.get()),
    )?;
    writeln!(output).context("failed to write session details")?;
    writeln!(output, "Attach:").context("failed to write session details")?;
    write_session_detail_row(output, "terminal", &native_attach_command(record))?;
    write_session_detail_row(
        output,
        "browser",
        &format!(
            "termstage session attach {} --backend {} --browser",
            record.id,
            record.backend.as_str()
        ),
    )
}

fn write_session_detail_row<W: Write>(
    output: &mut W,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    writeln!(output, "  {key:<12}  {value}").context("failed to write session details")
}

async fn create_backend_session(
    backend: CliBackend,
    backend_session: &SessionName,
    initial_size: TerminalSize,
    command: &BackendSessionCommand,
) -> anyhow::Result<()> {
    match (backend, command) {
        (CliBackend::Tmux, BackendSessionCommand::Tmux(command)) => {
            let tmux = TmuxBackend::from_path().context("failed to resolve tmux backend")?;
            tmux.create_new_session(backend_session, initial_size, command.as_ref())
                .await
                .with_context(|| {
                    format!("failed to create tmux session {}", backend_session.as_str())
                })?;
            Ok(())
        }
        (CliBackend::Rmux, BackendSessionCommand::Rmux(command)) => {
            let mut rmux = RmuxBackend::connect_or_start()
                .await
                .context("failed to connect or start rmux backend")?;
            rmux.create_new_session(backend_session, initial_size, command.as_ref())
                .await
                .with_context(|| {
                    format!("failed to create rmux session {}", backend_session.as_str())
                })?;
            Ok(())
        }
        (CliBackend::Tmux, BackendSessionCommand::Rmux(_))
        | (CliBackend::Rmux, BackendSessionCommand::Tmux(_)) => {
            bail!("backend command kind does not match requested backend")
        }
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
        CliBackend::Rmux => {
            let rmux = RmuxBackend::connect()
                .await
                .context("failed to connect to rmux backend")?;
            if !rmux
                .session_exists_by_name(backend_session)
                .await
                .with_context(|| {
                    format!("failed to check rmux session {}", backend_session.as_str())
                })?
            {
                return Ok(false);
            }
            rmux.kill_session_by_name(backend_session)
                .await
                .with_context(|| {
                    format!("failed to kill rmux session {}", backend_session.as_str())
                })
        }
    }
}

fn native_attach_command(record: &CliSessionRecord) -> String {
    match record.backend {
        CliBackend::Tmux => format!("tmux attach -t {}", record.backend_session),
        CliBackend::Rmux => format!("rmux attach -t {}", record.backend_session),
    }
}

fn native_attach_process(record: &CliSessionRecord) -> Command {
    let mut command = match record.backend {
        CliBackend::Tmux => Command::new("tmux"),
        CliBackend::Rmux => Command::new("rmux"),
    };
    command.arg("attach").arg("-t").arg(&record.backend_session);
    command
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
        if next_len > api_client::MAX_RESPONSE_BYTES {
            bail!(
                "API response exceeded {} bytes",
                api_client::MAX_RESPONSE_BYTES
            );
        }
        let Some(slice) = chunk.get(..bytes) else {
            bail!("API response chunk length was invalid");
        };
        output.extend_from_slice(slice);
    }
}

fn parse_http_response(response: &[u8]) -> anyhow::Result<ParsedHttpResponse> {
    let header_end = response
        .windows(api_client::HTTP_HEADER_SEPARATOR.len())
        .position(|window| window == api_client::HTTP_HEADER_SEPARATOR)
        .context("API response did not contain HTTP headers")?;
    let (header_bytes, body_bytes) = response.split_at(header_end);
    let body_bytes = body_bytes
        .get(api_client::HTTP_HEADER_SEPARATOR.len()..)
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
            .windows(api_client::HTTP_LINE_SEPARATOR.len())
            .position(|window| window == api_client::HTTP_LINE_SEPARATOR)
            .context("chunked response missing chunk size")?;
        let (size_bytes, rest) = body.split_at(line_end);
        let rest = rest
            .get(api_client::HTTP_LINE_SEPARATOR.len()..)
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
            .checked_add(api_client::HTTP_LINE_SEPARATOR.len())
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
        if separator != api_client::HTTP_LINE_SEPARATOR {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
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

    const fn as_backend(self) -> BackendScrollDirection {
        match self {
            Self::Up => BackendScrollDirection::Up,
            Self::Down => BackendScrollDirection::Down,
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

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn session_attach_args() -> SessionAttachArgs {
        SessionAttachArgs {
            session_id: "demo".to_owned(),
            backend: None,
            browser: false,
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            open: false,
            font_size: browser_presentation::DEFAULT_FONT_SIZE,
            theme: CliTheme::HighContrast,
            expose_public: false,
            public_url: None,
            token_env: None,
            base_path: None,
        }
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
        let args = SessionAttachArgs {
            host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            ..session_attach_args()
        };
        assert!(ValidatedSessionAttachConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_validate_public_exposure_config() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([3; 32]).to_url_token();
        let (exposure, parsed_token) = exposure_and_token_from_parts(
            true,
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            Some("https://term.example.com/"),
            Some("TERMSTAGE_TOKEN"),
            |name| {
                if name == "TERMSTAGE_TOKEN" {
                    Ok(token.clone())
                } else {
                    Err(env::VarError::NotPresent)
                }
            },
        )?;
        assert!(matches!(exposure, WebExposure::Public { .. }));
        assert!(parsed_token.constant_time_eq(&AccessToken::from_bytes([3; 32])));
        Ok(())
    }

    #[test]
    fn test_should_reject_public_mode_without_required_flags() {
        assert!(
            exposure_and_token_from_parts(
                true,
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                None,
                None,
                |_name| Err(env::VarError::NotPresent)
            )
            .is_err()
        );
    }

    #[test]
    fn test_should_reject_public_args_without_public_mode() {
        let args = SessionAttachArgs {
            public_url: Some("https://term.example.com/".to_owned()),
            ..session_attach_args()
        };
        assert!(ValidatedSessionAttachConfig::try_from(args).is_err());
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
        let args = SessionAttachArgs {
            session_id: "bad/name".to_owned(),
            ..session_attach_args()
        };
        assert!(ValidatedSessionAttachConfig::try_from(args).is_err());
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
    fn test_should_parse_session_attach_as_native_by_default() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from(["termstage", "session", "attach", "demo"])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::Attach(SessionAttachArgs {
                ref session_id,
                backend: None,
                browser: false,
                ..
            }) if session_id == "demo"
        ));
        Ok(())
    }

    #[test]
    fn test_should_parse_session_attach_browser_mode() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from([
            "termstage",
            "session",
            "attach",
            "demo",
            "--backend",
            "rmux",
            "--browser",
            "--open",
        ])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::Attach(SessionAttachArgs {
                ref session_id,
                backend: Some(CliBackend::Rmux),
                browser: true,
                open: true,
                ..
            }) if session_id == "demo"
        ));
        Ok(())
    }

    #[test]
    fn test_should_accept_session_attach_browser_typo_alias() -> anyhow::Result<()> {
        let args =
            CliArgs::try_parse_from(["termstage", "session", "attach", "demo", "--broswer"])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::Attach(SessionAttachArgs { browser: true, .. })
        ));
        Ok(())
    }

    #[test]
    fn test_should_parse_session_screen_command() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from([
            "termstage",
            "session",
            "screen",
            "demo",
            "--backend",
            "rmux",
        ])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::Screen {
                ref session_id,
                backend: Some(CliBackend::Rmux),
            } if session_id == "demo"
        ));
        Ok(())
    }

    #[test]
    fn test_should_parse_session_send_text_command() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from(["termstage", "session", "send-text", "demo", "hello"])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::SendText {
                ref session_id,
                backend: None,
                ref text,
            } if session_id == "demo" && text == "hello"
        ));
        Ok(())
    }

    #[test]
    fn test_should_parse_session_send_key_command() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from(["termstage", "session", "send-key", "demo", "Enter"])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::SendKey {
                ref session_id,
                backend: None,
                ref key,
            } if session_id == "demo" && key == "Enter"
        ));
        Ok(())
    }

    #[test]
    fn test_should_parse_session_exec_command_after_separator() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from([
            "termstage",
            "session",
            "exec",
            "demo",
            "--backend",
            "tmux",
            "--wait-for",
            "done",
            "--capture",
            "--",
            "echo",
            "done",
        ])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::Exec {
                ref session_id,
                backend: Some(CliBackend::Tmux),
                ref command,
                ref wait_for,
                capture: true,
                ..
            } if session_id == "demo"
                && command == &["echo".to_owned(), "done".to_owned()]
                && wait_for.as_deref() == Some("done")
        ));
        Ok(())
    }

    #[test]
    fn test_should_parse_session_run_command_alias() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from([
            "termstage",
            "session",
            "run-command",
            "demo",
            "--",
            "echo",
            "ok",
        ])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::Exec {
                ref session_id,
                ref command,
                ..
            } if session_id == "demo" && command == &["echo".to_owned(), "ok".to_owned()]
        ));
        Ok(())
    }

    #[test]
    fn test_should_parse_session_scroll_command() -> anyhow::Result<()> {
        let args =
            CliArgs::try_parse_from(["termstage", "session", "scroll", "demo", "down", "5"])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::Scroll {
                ref session_id,
                backend: None,
                direction: CliScrollDirection::Down,
                amount: 5,
            } if session_id == "demo"
        ));
        Ok(())
    }

    #[test]
    fn test_should_map_session_send_key_semantic_tokens() -> anyhow::Result<()> {
        assert_eq!(local_semantic_key_token("Enter")?, "Enter");
        assert_eq!(local_semantic_key_token("CtrlC")?, "C-c");
        assert_eq!(local_semantic_key_token("CtrlD")?, "C-d");
        assert_eq!(local_semantic_key_token("ArrowUp")?, "Up");
        assert_eq!(local_semantic_key_token("a")?, "a");
        assert!(local_semantic_key_token("").is_err());
        assert!(local_semantic_key_token("C-c").is_err());
        assert!(local_semantic_key_token("\n").is_err());
        Ok(())
    }

    #[test]
    fn test_should_render_screen_snapshot_as_json() -> anyhow::Result<()> {
        let snapshot = BackendScreenSnapshot::new_with_cursor_visibility(
            TerminalSize::new(80, 24)?,
            4,
            3,
            false,
            vec!["prompt".to_owned()],
        );
        let response = screen_json(&snapshot);

        assert_eq!(response["size"]["cols"], 80);
        assert_eq!(response["cursorCol"], 4);
        assert_eq!(response["cursorVisible"], false);
        assert_eq!(response["lines"], json!(["prompt"]));
        Ok(())
    }

    #[test]
    fn test_should_validate_session_exec_command_text() -> anyhow::Result<()> {
        assert_eq!(
            local_exec_command(&["echo".to_owned(), "ok".to_owned()])?,
            "echo ok"
        );
        assert!(local_exec_command(&["bad\ncommand".to_owned()]).is_err());
        Ok(())
    }

    #[test]
    fn test_should_validate_session_scroll_amount() -> anyhow::Result<()> {
        assert_eq!(validate_local_scroll_amount(1)?, 1);
        assert_eq!(
            validate_local_scroll_amount(semantic_api::SCROLL_MAX_AMOUNT)?,
            semantic_api::SCROLL_MAX_AMOUNT
        );
        assert!(validate_local_scroll_amount(0).is_err());
        assert!(validate_local_scroll_amount(semantic_api::SCROLL_MAX_AMOUNT + 1).is_err());
        Ok(())
    }

    #[test]
    fn test_should_reject_browser_only_flags_without_browser_mode() {
        assert!(
            CliArgs::try_parse_from(["termstage", "session", "attach", "demo", "--open"]).is_err()
        );
        assert!(
            CliArgs::try_parse_from([
                "termstage",
                "session",
                "attach",
                "demo",
                "--font-size",
                "28",
            ])
            .is_err()
        );
    }

    #[test]
    fn test_should_parse_rmux_session_create_command() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from([
            "termstage",
            "session",
            "create",
            "--backend",
            "rmux",
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
                backend: CliBackend::Rmux,
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
    fn test_should_format_session_list_as_aligned_table() -> anyhow::Result<()> {
        let records = [
            CliSessionRecord {
                id: "TerminalUse-yoyo".to_owned(),
                backend: CliBackend::Tmux,
                display_name: "yoyo".to_owned(),
                backend_session: "TerminalUse-yoyo".to_owned(),
            },
            CliSessionRecord {
                id: "ts-short".to_owned(),
                backend: CliBackend::Tmux,
                display_name: "longer-name".to_owned(),
                backend_session: "ts-longer-name".to_owned(),
            },
        ];
        let mut output = Vec::new();

        write_session_list(&mut output, &records)?;

        let text = String::from_utf8(output)?;
        assert_eq!(
            text,
            "SESSION_ID        BACKEND  DISPLAY_NAME\n\
             TerminalUse-yoyo  tmux     yoyo\n\
             ts-short          tmux     longer-name\n"
        );
        Ok(())
    }

    #[test]
    fn test_should_format_session_inspect_as_sections() -> anyhow::Result<()> {
        let details = CliSessionInspectDetails {
            record: CliSessionRecord {
                id: "TerminalUse-yoyo".to_owned(),
                backend: CliBackend::Rmux,
                display_name: "yoyo".to_owned(),
                backend_session: "TerminalUse-yoyo".to_owned(),
            },
            window: "0".to_owned(),
            pane: "%0".to_owned(),
            size: TerminalSize::new(181, 44)?,
        };
        let mut output = Vec::new();

        write_session_inspect_details(&mut output, &details)?;

        let text = String::from_utf8(output)?;
        assert_eq!(
            text,
            concat!(
                "Session:\n",
                "  id            TerminalUse-yoyo\n",
                "  backend       rmux\n",
                "  display-name  yoyo\n",
                "\n",
                "Properties:\n",
                "  window        0\n",
                "  pane          %0\n",
                "  size          181x44\n",
                "\n",
                "Attach:\n",
                "  terminal      rmux attach -t TerminalUse-yoyo\n",
                "  browser       termstage session attach TerminalUse-yoyo --backend rmux \
                 --browser\n",
            )
        );
        Ok(())
    }

    #[test]
    fn test_should_prefix_termstage_created_backend_session_names() -> anyhow::Result<()> {
        assert_eq!(
            termstage_tmux_session_name("abc")?.as_str(),
            "TerminalUse-abc"
        );
        assert_eq!(
            termstage_rmux_session_name("abc")?.as_str(),
            "TerminalUse-abc"
        );
        assert_eq!(
            termstage_tmux_session_name("TerminalUse-abc")?.as_str(),
            "TerminalUse-abc"
        );
        assert_eq!(
            termstage_tmux_session_name("ts-abc")?.as_str(),
            "TerminalUse-ts-abc"
        );
        Ok(())
    }

    #[test]
    fn test_should_derive_cli_record_from_tmux_session() -> anyhow::Result<()> {
        let record = cli_record_from_tmux_session(&SessionName::from_str("TerminalUse-yoyo")?);

        assert_eq!(record.id, "TerminalUse-yoyo");
        assert_eq!(record.backend, CliBackend::Tmux);
        assert_eq!(record.display_name, "yoyo");
        assert_eq!(record.backend_session, "TerminalUse-yoyo");
        let legacy = cli_record_from_tmux_session(&SessionName::from_str("ts-yoyo")?);
        assert_eq!(legacy.display_name, "yoyo");
        Ok(())
    }

    #[test]
    fn test_should_derive_cli_record_from_rmux_session() -> anyhow::Result<()> {
        let record = cli_record_from_rmux_session(&SessionName::from_str("TerminalUse-demo")?);

        assert_eq!(record.id, "TerminalUse-demo");
        assert_eq!(record.backend, CliBackend::Rmux);
        assert_eq!(record.display_name, "demo");
        assert_eq!(record.backend_session, "TerminalUse-demo");
        Ok(())
    }

    #[test]
    fn test_should_parse_session_stop_command() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from(["termstage", "session", "stop", "demo"])?;
        let command = ValidatedCliCommand::try_from(args)?;
        let ValidatedCliCommand::Session(args) = command else {
            anyhow::bail!("session command group must validate to session args");
        };
        assert!(matches!(
            args.command,
            SessionCommand::Stop { ref session_id } if session_id == "demo"
        ));
        Ok(())
    }

    #[test]
    fn test_should_reject_removed_session_stop_flags() {
        assert!(
            CliArgs::try_parse_from(["termstage", "session", "stop", "demo", "--detach"]).is_err()
        );
        assert!(
            CliArgs::try_parse_from(["termstage", "session", "stop", "demo", "--kill"]).is_err()
        );
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
    fn test_should_parse_auth_command_group() -> anyhow::Result<()> {
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
    fn test_should_reject_removed_web_and_browser_command_groups() {
        assert!(CliArgs::try_parse_from(["termstage", "web", "token", "generate"]).is_err());
        assert!(CliArgs::try_parse_from(["termstage", "web", "attach", "demo"]).is_err());
        assert!(CliArgs::try_parse_from(["termstage", "browser", "attach", "demo"]).is_err());
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
    fn test_should_reject_invalid_font_size() {
        let args = SessionAttachArgs {
            font_size: browser_presentation::FONT_SIZE_MAX.saturating_add(1),
            ..session_attach_args()
        };
        assert!(ValidatedSessionAttachConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_validate_base_path_argument() -> anyhow::Result<()> {
        let args = SessionAttachArgs {
            base_path: Some("/p/sess-1/".to_owned()),
            ..session_attach_args()
        };
        let config = ValidatedSessionAttachConfig::try_from(args)?;
        assert_eq!(
            config.base_path.as_ref().map(BasePath::as_str),
            Some("/p/sess-1/")
        );
        Ok(())
    }

    #[test]
    fn test_should_reject_invalid_base_path_argument() {
        let args = SessionAttachArgs {
            base_path: Some("p/missing-leading-slash/".to_owned()),
            ..session_attach_args()
        };
        assert!(ValidatedSessionAttachConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_accept_non_root_test_process() -> anyhow::Result<()> {
        if !rustix::process::geteuid().is_root() {
            reject_root_user()?;
        }
        Ok(())
    }
}
