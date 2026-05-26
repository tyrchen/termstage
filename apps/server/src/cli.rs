//! Command-line interface for browser terminal mode.

use std::{
    env,
    ffi::OsString,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    str::FromStr,
    time::Duration,
};

use anyhow::{Context, bail};
use clap::{Parser, ValueEnum};
use termstage_core::{
    protocol::{AccessToken, SessionName, TerminalSize},
    runtime::{
        ExitPolicy, ReconnectPolicy, RuntimeConfig, RuntimeSession, SessionMode, ShellCommand,
        ShutdownReason,
    },
    security::{BasePath, PublicBaseUrl},
};
use tracing::info;

use crate::{
    local_terminal,
    web::{PresentationSettings, PresentationTheme, WebConfig, WebExposure, serve},
};

const DEFAULT_SESSION: &str = "presentation";
const DEFAULT_FONT_SIZE: u16 = 24;
const MIN_FONT_SIZE: u16 = 12;
const MAX_FONT_SIZE: u16 = 96;
const TOKEN_ENV_MAX_BYTES: usize = 128;
const ACTOR_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Browser terminal command-line arguments.
#[derive(Debug, Parser)]
#[command(name = "termstage")]
#[command(about = "Run a browser terminal for presentations")]
#[command(
    long_about = "Run a browser terminal for presentations. By default the server is \
                  loopback-only. Public pod exposure requires --expose-public, --public-url, and \
                  --token-env. This is a shell bridge, not a sandbox: browser input is sent to a \
                  real shell or tmux session with the current OS user's privileges."
)]
pub struct CliArgs {
    /// Attach to or create this tmux session.
    #[arg(long, default_value = DEFAULT_SESSION)]
    session: String,
    /// Terminal backend mode.
    #[arg(long, value_enum, default_value_t = CliMode::Tmux)]
    mode: CliMode,
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
    /// Attach the invoking terminal as a local frontend in shell mode.
    #[arg(short = 'a', long, default_value_t = false)]
    attach_local_terminal: bool,
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
    /// Whether to attach the invoking terminal as the local frontend.
    pub attach_local_terminal: bool,
}

impl TryFrom<CliArgs> for ValidatedCliConfig {
    type Error = anyhow::Error;

    fn try_from(args: CliArgs) -> Result<Self, Self::Error> {
        if !(MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&args.font_size) {
            bail!("font size must be in {MIN_FONT_SIZE}..={MAX_FONT_SIZE}");
        }
        let base_path = match args.base_path.as_deref() {
            Some(value) => Some(BasePath::from_str(value).context("invalid --base-path")?),
            None => None,
        };
        let (exposure, token) = exposure_and_token(&args)?;
        let initial_size = TerminalSize::new(80, 24).context("default terminal size is invalid")?;
        if args.attach_local_terminal && !matches!(args.mode, CliMode::Shell) {
            bail!("--attach-local-terminal requires --mode shell");
        }
        if args.command.is_some() && !matches!(args.mode, CliMode::Shell) {
            bail!("--command requires --mode shell");
        }
        if !args.command_args.is_empty() && !matches!(args.mode, CliMode::Shell) {
            bail!("--command-arg requires --mode shell");
        }
        let explicit_shell_command = args.command.is_some();
        let mode = match args.mode {
            CliMode::Tmux => SessionMode::Tmux {
                session: SessionName::from_str(&args.session)
                    .context("invalid tmux session name")?,
            },
            CliMode::Shell => SessionMode::NewShell {
                shell: shell_mode_command(args.command, args.command_args)?,
            },
        };
        let exit_policy = match args.exit_policy {
            Some(policy) => policy.into(),
            None if args.attach_local_terminal || explicit_shell_command => ExitPolicy::End,
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
            attach_local_terminal: args.attach_local_terminal,
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
    let config = ValidatedCliConfig::try_from(args)?;
    run_with_config(config).await
}

/// Runs the browser terminal from a validated configuration.
///
/// # Errors
///
/// Returns an error when runtime or server startup fails.
pub async fn run_with_config(mut config: ValidatedCliConfig) -> anyhow::Result<()> {
    reject_root_user()?;
    if config.attach_local_terminal
        && let Some(size) =
            local_terminal::current_terminal_size().context("failed to read local terminal size")?
    {
        config.runtime.initial_size = size;
    }
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
    let shutdown_reason = if config.attach_local_terminal {
        local_terminal::run(session.command_sender())
            .await
            .context("local terminal frontend failed")?
    } else {
        wait_for_shutdown_or_runtime_exit(&session).await
    };
    server
        .shutdown()
        .await
        .context("failed to shutdown browser terminal server")?;
    session
        .shutdown(shutdown_reason)
        .await
        .context("failed to shutdown browser terminal runtime")
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

fn exposure_and_token(args: &CliArgs) -> anyhow::Result<(WebExposure, AccessToken)> {
    exposure_and_token_with_env(args, |name| env::var(name))
}

fn exposure_and_token_with_env(
    args: &CliArgs,
    get_env: impl Fn(&str) -> Result<String, env::VarError>,
) -> anyhow::Result<(WebExposure, AccessToken)> {
    if args.expose_public {
        let public_url = args
            .public_url
            .as_deref()
            .context("--public-url is required with --expose-public")
            .and_then(|value| {
                value
                    .parse::<PublicBaseUrl>()
                    .context("invalid --public-url for public exposure")
            })?;
        let token_env = args
            .token_env
            .as_deref()
            .context("--token-env is required with --expose-public")?;
        validate_token_env_name(token_env)?;
        let token_value = get_env(token_env)
            .with_context(|| format!("failed to read access token from ${token_env}"))?;
        let token = AccessToken::from_str(&token_value)
            .with_context(|| format!("invalid access token in ${token_env}"))?;
        Ok((WebExposure::Public { public_url }, token))
    } else {
        if !args.host.is_loopback() {
            bail!("browser terminal bind host must be loopback unless --expose-public is set");
        }
        if args.public_url.is_some() {
            bail!("--public-url requires --expose-public");
        }
        if args.token_env.is_some() {
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

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliMode {
    Tmux,
    Shell,
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

impl Default for CliArgs {
    fn default() -> Self {
        Self {
            session: DEFAULT_SESSION.to_owned(),
            mode: CliMode::Tmux,
            command: None,
            command_args: Vec::new(),
            attach_local_terminal: false,
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
        let config = ValidatedCliConfig::try_from(CliArgs::default())?;
        assert!(config.host.is_loopback());
        assert_eq!(config.port, 0);
        assert_eq!(config.presentation.font_size, DEFAULT_FONT_SIZE);
        assert!(matches!(config.exposure, WebExposure::Local));
        assert!(matches!(config.runtime.mode, SessionMode::Tmux { .. }));
        assert_eq!(config.runtime.exit_policy, ExitPolicy::Hold);
        assert!(!config.attach_local_terminal);
        Ok(())
    }

    #[test]
    fn test_should_reject_non_loopback_host() {
        let args = CliArgs {
            host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            ..CliArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_validate_public_exposure_config() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([3; 32]).to_url_token();
        let args = CliArgs {
            host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: 8080,
            expose_public: true,
            public_url: Some("https://term.example.com/".to_owned()),
            token_env: Some("TERMSTAGE_TOKEN".to_owned()),
            ..CliArgs::default()
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
        let args = CliArgs {
            expose_public: true,
            ..CliArgs::default()
        };
        assert!(
            exposure_and_token_with_env(&args, |_name| Err(env::VarError::NotPresent)).is_err()
        );
    }

    #[test]
    fn test_should_reject_public_args_without_public_mode() {
        let args = CliArgs {
            public_url: Some("https://term.example.com/".to_owned()),
            ..CliArgs::default()
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
        let args = CliArgs {
            session: "bad/name".to_owned(),
            ..CliArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_validate_shell_mode_without_session_use() -> anyhow::Result<()> {
        let args = CliArgs {
            mode: CliMode::Shell,
            command: Some(PathBuf::from("/bin/sh")),
            ..CliArgs::default()
        };
        let config = ValidatedCliConfig::try_from(args)?;
        assert!(matches!(config.runtime.mode, SessionMode::NewShell { .. }));
        assert_eq!(config.runtime.exit_policy, ExitPolicy::End);
        Ok(())
    }

    #[test]
    fn test_should_allow_explicit_shell_command_hold_policy() -> anyhow::Result<()> {
        let args = CliArgs {
            mode: CliMode::Shell,
            command: Some(PathBuf::from("/bin/sh")),
            exit_policy: Some(CliExitPolicy::Hold),
            ..CliArgs::default()
        };
        let config = ValidatedCliConfig::try_from(args)?;
        assert_eq!(config.runtime.exit_policy, ExitPolicy::Hold);
        Ok(())
    }

    #[test]
    fn test_should_validate_attach_local_terminal_shell_mode() -> anyhow::Result<()> {
        let args = CliArgs {
            mode: CliMode::Shell,
            command: Some(PathBuf::from("/bin/sh")),
            attach_local_terminal: true,
            ..CliArgs::default()
        };
        let config = ValidatedCliConfig::try_from(args)?;
        assert!(config.attach_local_terminal);
        assert_eq!(config.runtime.exit_policy, ExitPolicy::End);
        assert!(matches!(config.runtime.mode, SessionMode::NewShell { .. }));
        Ok(())
    }

    #[test]
    fn test_should_pass_shell_arguments_to_shell_mode() -> anyhow::Result<()> {
        let args = CliArgs {
            mode: CliMode::Shell,
            command: Some(PathBuf::from("codemax")),
            command_args: vec![OsString::from("claude")],
            ..CliArgs::default()
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
    fn test_should_reject_attach_local_terminal_outside_shell_mode() {
        let args = CliArgs {
            attach_local_terminal: true,
            ..CliArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_reject_command_args_outside_shell_mode() {
        let args = CliArgs {
            command_args: vec![OsString::from("claude")],
            ..CliArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_reject_command_outside_shell_mode() {
        let args = CliArgs {
            command: Some(PathBuf::from("claude")),
            ..CliArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_parse_short_command_args() -> anyhow::Result<()> {
        let args = CliArgs::try_parse_from([
            "termstage",
            "--mode",
            "shell",
            "--command",
            "abc",
            "-g=-p",
            "-g=--resume",
        ])?;
        let config = ValidatedCliConfig::try_from(args)?;
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
    fn test_should_parse_short_attach_local_terminal_flag() -> anyhow::Result<()> {
        let args =
            CliArgs::try_parse_from(["termstage", "--mode", "shell", "--command", "abc", "-a"])?;
        let config = ValidatedCliConfig::try_from(args)?;
        assert!(config.attach_local_terminal);
        assert_eq!(config.runtime.exit_policy, ExitPolicy::End);
        Ok(())
    }

    #[test]
    fn test_should_reject_invalid_font_size() {
        let args = CliArgs {
            font_size: MAX_FONT_SIZE.saturating_add(1),
            ..CliArgs::default()
        };
        assert!(ValidatedCliConfig::try_from(args).is_err());
    }

    #[test]
    fn test_should_validate_base_path_argument() -> anyhow::Result<()> {
        let args = CliArgs {
            base_path: Some("/p/sess-1/".to_owned()),
            ..CliArgs::default()
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
        let args = CliArgs {
            base_path: Some("p/missing-leading-slash/".to_owned()),
            ..CliArgs::default()
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
