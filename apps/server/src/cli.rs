//! Command-line interface for browser terminal mode.

use std::{
    ffi::OsString,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    str::FromStr,
};

use anyhow::{Context, bail};
use clap::{Parser, ValueEnum};
use presenterm_core::{
    protocol::{AccessToken, SessionName, TerminalSize},
    runtime::{
        ReconnectPolicy, RuntimeConfig, RuntimeSession, SessionMode, ShellCommand, ShutdownReason,
    },
};
use tracing::info;

use crate::web::{PresentationSettings, PresentationTheme, WebConfig, serve};

const DEFAULT_SESSION: &str = "presentation";
const DEFAULT_FONT_SIZE: u16 = 24;
const MIN_FONT_SIZE: u16 = 12;
const MAX_FONT_SIZE: u16 = 96;

/// Browser terminal command-line arguments.
#[derive(Debug, Parser)]
#[command(name = "presenterm")]
#[command(about = "Run a local browser terminal for presentations")]
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
    /// Loopback bind address.
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
    /// Session keepalive policy.
    #[arg(long, value_enum, default_value_t = CliKeepalive::Session)]
    keepalive: CliKeepalive,
}

/// Validated CLI configuration.
#[derive(Debug, Clone)]
pub struct ValidatedCliConfig {
    /// Runtime configuration.
    pub runtime: RuntimeConfig,
    /// Loopback bind host.
    pub host: IpAddr,
    /// Loopback bind port.
    pub port: u16,
    /// Whether to open the browser.
    pub open: bool,
    /// Browser presentation settings.
    pub presentation: PresentationSettings,
}

impl TryFrom<CliArgs> for ValidatedCliConfig {
    type Error = anyhow::Error;

    fn try_from(args: CliArgs) -> Result<Self, Self::Error> {
        if !args.host.is_loopback() {
            bail!("browser terminal bind host must be loopback");
        }
        if !(MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&args.font_size) {
            bail!("font size must be in {MIN_FONT_SIZE}..={MAX_FONT_SIZE}");
        }
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
    let token = AccessToken::generate().context("failed to generate browser access token")?;
    let mut web_config = WebConfig::local(token, session.command_sender(), config.runtime);
    web_config.host = config.host;
    web_config.port = config.port;
    web_config.presentation = config.presentation;

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

async fn wait_for_shutdown() {
    let _result = tokio::signal::ctrl_c().await;
}

fn init_tracing() {
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_error| tracing_subscriber::EnvFilter::new("presenterm=info")),
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
