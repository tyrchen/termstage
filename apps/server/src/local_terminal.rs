//! Local terminal frontend for command wrapper mode.

#[allow(
    clippy::disallowed_types,
    reason = "local terminal mode setup and restoration run at the synchronous terminal boundary; \
              tokio::process::Command cannot be used from Drop"
)]
use std::process::Command as BlockingCommand;
use std::{
    io::{self, IsTerminal, Read, Write},
    process::Stdio,
    thread,
};

use anyhow::{Context, bail};
use bytes::Bytes;
use termstage_core::{
    protocol::{ServerControlMessage, TerminalSize},
    runtime::{ClientOutput, RuntimeCommand, RuntimeSession, ShutdownReason},
};
use tokio::sync::mpsc;
use tracing::debug;

const STTY_STATE_MAX_BYTES: usize = 1024;

/// Runs the local terminal frontend until the runtime closes it.
///
/// # Errors
///
/// Returns an error when terminal mode setup, runtime attachment, local input, or
/// local output fails.
pub async fn run(commands: mpsc::Sender<RuntimeCommand>) -> anyhow::Result<ShutdownReason> {
    let _terminal_mode =
        TerminalModeGuard::activate().context("failed to enter raw terminal mode")?;
    let (output_tx, output_rx) = RuntimeSession::client_mailbox();
    commands
        .send(RuntimeCommand::AttachTerminal { output: output_tx })
        .await
        .context("failed to attach local terminal to runtime")?;

    spawn_input_reader(commands.clone())?;
    spawn_resize_watcher(commands);
    let output_task = tokio::task::spawn_blocking(move || output_loop(output_rx));
    output_task
        .await
        .context("local terminal output task panicked")?
}

/// Reads the current local terminal size.
///
/// # Errors
///
/// Returns an error when `stty size` fails or reports invalid dimensions.
pub fn current_terminal_size() -> anyhow::Result<Option<TerminalSize>> {
    if !io::stdin().is_terminal() {
        return Ok(None);
    }
    read_stty_size().map(Some)
}

fn spawn_input_reader(commands: mpsc::Sender<RuntimeCommand>) -> anyhow::Result<()> {
    thread::Builder::new()
        .name("termstage-local-terminal-input".to_owned())
        .spawn(move || {
            let stdin = io::stdin();
            let mut stdin = stdin.lock();
            let mut buffer = [0_u8; 8192];
            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        let Some(bytes) = buffer.get(..read) else {
                            break;
                        };
                        let command = RuntimeCommand::TerminalInput {
                            bytes: Bytes::copy_from_slice(bytes),
                        };
                        if commands.blocking_send(command).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        debug!(%error, "local terminal input stopped");
                        break;
                    }
                }
            }
        })
        .context("failed to spawn local terminal input thread")?;
    Ok(())
}

#[cfg(unix)]
fn spawn_resize_watcher(commands: mpsc::Sender<RuntimeCommand>) {
    tokio::spawn(async move {
        let signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change());
        let Ok(mut signal) = signal else {
            return;
        };
        while signal.recv().await.is_some() {
            let Ok(Some(size)) = current_terminal_size() else {
                continue;
            };
            if commands
                .send(RuntimeCommand::Resize { size })
                .await
                .is_err()
            {
                break;
            }
        }
    });
}

#[cfg(not(unix))]
fn spawn_resize_watcher(_commands: mpsc::Sender<RuntimeCommand>) {}

fn output_loop(
    mut output_rx: termstage_core::runtime::ClientOutputRx,
) -> anyhow::Result<ShutdownReason> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    while let Some(output) = output_rx.blocking_recv() {
        match output {
            ClientOutput::Bytes(bytes) => {
                stdout
                    .write_all(&bytes)
                    .and_then(|()| stdout.flush())
                    .context("failed to write runtime output to local terminal")?;
            }
            ClientOutput::Control(
                ServerControlMessage::LeaseChanged { .. }
                | ServerControlMessage::Ready { .. }
                | ServerControlMessage::ReplayFinished
                | ServerControlMessage::ReplayStarted
                | ServerControlMessage::SizeChanged { .. },
            ) => {}
            ClientOutput::Control(ServerControlMessage::ProcessExited { message }) => {
                writeln!(stderr, "\r\n[termstage: {}]", message.as_str())
                    .context("failed to write process exit notice")?;
            }
            ClientOutput::Control(ServerControlMessage::Warning { message, .. }) => {
                writeln!(stderr, "\r\n[termstage warning: {}]", message.as_str())
                    .context("failed to write warning notice")?;
            }
            ClientOutput::Control(ServerControlMessage::Error { message, .. }) => {
                writeln!(stderr, "\r\n[termstage error: {}]", message.as_str())
                    .context("failed to write error notice")?;
            }
            ClientOutput::Closed(reason) => return Ok(reason),
        }
    }
    Ok(ShutdownReason::Supervisor)
}

#[derive(Debug)]
struct TerminalModeGuard {
    original_state: Option<String>,
}

impl TerminalModeGuard {
    fn activate() -> anyhow::Result<Self> {
        if !io::stdin().is_terminal() {
            return Ok(Self {
                original_state: None,
            });
        }

        let original_state = read_stty_state()?;
        set_stty_raw()?;
        Ok(Self {
            original_state: Some(original_state),
        })
    }
}

impl Drop for TerminalModeGuard {
    #[allow(
        clippy::disallowed_types,
        reason = "terminal mode restoration runs from Drop and cannot await \
                  tokio::process::Command"
    )]
    fn drop(&mut self) {
        if let Some(state) = &self.original_state {
            let result = BlockingCommand::new("stty")
                .arg(state)
                .stdin(Stdio::inherit())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if let Err(error) = result {
                debug!(%error, "failed to restore terminal mode");
            }
        }
    }
}

#[allow(
    clippy::disallowed_types,
    reason = "raw terminal setup is a short synchronous boundary operation before local IO starts"
)]
fn read_stty_state() -> anyhow::Result<String> {
    let output = BlockingCommand::new("stty")
        .arg("-g")
        .stdin(Stdio::inherit())
        .stderr(Stdio::null())
        .output()
        .context("failed to read current terminal mode")?;
    if !output.status.success() {
        let status = output.status;
        bail!("stty -g exited with {status}");
    }
    let state = String::from_utf8(output.stdout).context("stty state was not utf-8")?;
    let state = state.trim().to_owned();
    if state.is_empty() || state.len() > STTY_STATE_MAX_BYTES {
        bail!("stty returned invalid terminal state");
    }
    Ok(state)
}

#[allow(
    clippy::disallowed_types,
    reason = "terminal size probing is a short synchronous boundary operation before local IO \
              starts"
)]
fn read_stty_size() -> anyhow::Result<TerminalSize> {
    let output = BlockingCommand::new("stty")
        .arg("size")
        .stdin(Stdio::inherit())
        .stderr(Stdio::null())
        .output()
        .context("failed to read current terminal size")?;
    if !output.status.success() {
        let status = output.status;
        bail!("stty size exited with {status}");
    }
    let value = String::from_utf8(output.stdout).context("stty size output was not utf-8")?;
    parse_stty_size(value.trim())
}

fn parse_stty_size(value: &str) -> anyhow::Result<TerminalSize> {
    let mut parts = value.split_ascii_whitespace();
    let rows = parts
        .next()
        .context("stty size did not include rows")?
        .parse::<u16>()
        .context("stty size rows were invalid")?;
    let cols = parts
        .next()
        .context("stty size did not include columns")?
        .parse::<u16>()
        .context("stty size columns were invalid")?;
    if parts.next().is_some() {
        bail!("stty size returned extra fields");
    }
    TerminalSize::new(cols, rows).context("stty size was outside supported terminal bounds")
}

#[allow(
    clippy::disallowed_types,
    reason = "raw terminal setup is a short synchronous boundary operation before local IO starts"
)]
fn set_stty_raw() -> anyhow::Result<()> {
    let status = BlockingCommand::new("stty")
        .args(["raw", "-echo"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to set raw terminal mode")?;
    if status.success() {
        Ok(())
    } else {
        bail!("stty raw -echo exited with {status}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_parse_stty_size() -> anyhow::Result<()> {
        let size = parse_stty_size("48 160")?;
        assert_eq!(size.rows.get(), 48);
        assert_eq!(size.cols.get(), 160);
        Ok(())
    }

    #[test]
    fn test_should_reject_invalid_stty_size() {
        assert!(parse_stty_size("").is_err());
        assert!(parse_stty_size("48").is_err());
        assert!(parse_stty_size("48 160 extra").is_err());
        assert!(parse_stty_size("4 160").is_err());
    }
}
