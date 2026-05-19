//! Command-line interface for browser terminal mode.

use std::{
    env,
    ffi::OsString,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    str::FromStr,
};

use anyhow::{Context, bail};
use clap::{Parser, ValueEnum};
use termstage_core::{
    protocol::{AccessToken, SessionName, TerminalSize},
    runtime::{
        ReconnectPolicy, RuntimeConfig, RuntimeSession, SessionMode, ShellCommand, ShutdownReason,
    },
    security::PublicBaseUrl,
};
use tracing::info;

use crate::web::{PresentationSettings, PresentationTheme, WebConfig, WebExposure, serve};

const DEFAULT_SESSION: &str = "presentation";
const DEFAULT_FONT_SIZE: u16 = 24;
const MIN_FONT_SIZE: u16 = 12;
const MAX_FONT_SIZE: u16 = 96;
const TOKEN_ENV_MAX_BYTES: usize = 128;

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
    /// Shell executable for shell mode.
    #[arg(long)]
    shell: Option<PathBuf>,
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
    /// Enable internet-facing pod mode behind an HTTPS ingress.
    #[arg(long, default_value_t = false)]
    expose_public: bool,
    /// Browser-visible HTTPS base URL for public mode.
    #[arg(long)]
    public_url: Option<String>,
    /// Environment variable containing the 64-hex-character access token.
    #[arg(long)]
    token_env: Option<String>,
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
}

impl TryFrom<CliArgs> for ValidatedCliConfig {
    type Error = anyhow::Error;

    fn try_from(args: CliArgs) -> Result<Self, Self::Error> {
        if !(MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&args.font_size) {
            bail!("font size must be in {MIN_FONT_SIZE}..={MAX_FONT_SIZE}");
        }
        let (exposure, token) = exposure_and_token(&args)?;
        let initial_size = TerminalSize::new(80, 24).context("default terminal size is invalid")?;
        let mode = match args.mode {
            CliMode::Tmux => SessionMode::Tmux {
                session: SessionName::from_str(&args.session)
                    .context("invalid tmux session name")?,
            },
            CliMode::Shell => SessionMode::NewShell {
                shell: shell_command(args.shell)?,
            },
        };
        Ok(Self {
            runtime: RuntimeConfig {
                mode,
                initial_size,
                reconnect_policy: args.keepalive.into(),
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
pub async fn run_with_config(config: ValidatedCliConfig) -> anyhow::Result<()> {
    reject_root_user()?;
    let session = RuntimeSession::start(config.runtime.clone())
        .context("failed to start browser terminal runtime")?;
    let mut web_config = WebConfig::local(config.token, session.command_sender(), config.runtime);
    web_config.host = config.host;
    web_config.port = config.port;
    web_config.presentation = config.presentation;
    web_config.exposure = config.exposure;

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
    wait_for_shutdown().await;
    server
        .shutdown()
        .await
        .context("failed to shutdown browser terminal server")?;
    session
        .shutdown(ShutdownReason::Supervisor)
        .await
        .context("failed to shutdown browser terminal runtime")
}

fn shell_command(path: Option<PathBuf>) -> anyhow::Result<ShellCommand> {
    match path {
        Some(path) => ShellCommand::new(path, Vec::<OsString>::new()).map_err(Into::into),
        None => ShellCommand::default_unix().map_err(Into::into),
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

async fn wait_for_shutdown() {
    let _result = tokio::signal::ctrl_c().await;
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

impl Default for CliArgs {
    fn default() -> Self {
        Self {
            session: DEFAULT_SESSION.to_owned(),
            mode: CliMode::Tmux,
            shell: None,
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            open: false,
            font_size: DEFAULT_FONT_SIZE,
            theme: CliTheme::HighContrast,
            keepalive: CliKeepalive::Session,
            expose_public: false,
            public_url: None,
            token_env: None,
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
            shell: Some(PathBuf::from("/bin/sh")),
            ..CliArgs::default()
        };
        let config = ValidatedCliConfig::try_from(args)?;
        assert!(matches!(config.runtime.mode, SessionMode::NewShell { .. }));
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
    fn test_should_accept_non_root_test_process() -> anyhow::Result<()> {
        if !rustix::process::geteuid().is_root() {
            reject_root_user()?;
        }
        Ok(())
    }
}
