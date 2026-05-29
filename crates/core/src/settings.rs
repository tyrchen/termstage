//! Core settings grouped by runtime and backend ownership.

use std::time::Duration;

/// Local PTY runtime actor settings.
pub(crate) mod runtime_actor {
    use super::Duration;

    /// Bounded mailbox capacity for runtime actor commands.
    pub(crate) const COMMAND_MAILBOX_CAPACITY: usize = 128;
    /// Bounded output channel capacity for each browser client.
    pub(crate) const CLIENT_OUTPUT_CAPACITY: usize = 256;
    /// Blocking PTY reader chunk size.
    pub(crate) const PTY_READ_CHUNK_SIZE: usize = 8192;
    /// Maximum replay buffer bytes kept for reconnecting clients.
    pub(crate) const REPLAY_BUFFER_BYTES: usize = 1024 * 1024;
    /// Number of output chunks retained for replay.
    pub(crate) const REPLAY_BUFFER_CHUNKS: usize = CLIENT_OUTPUT_CAPACITY / 2;
    /// Actor wait timeout used when no runtime command is available.
    pub(crate) const ACTOR_IDLE_WAIT: Duration = Duration::from_millis(10);
    /// `TERM` value exported into local PTY runtime commands.
    pub(crate) const TERMINAL_TERM: &str = "xterm-256color";
}

/// tmux backend process defaults.
pub(crate) mod tmux_backend {
    /// `COLORTERM` value exported into termstage-created tmux sessions.
    pub(crate) const TERMINAL_COLOR_MODE: &str = "truecolor";
    /// `TERM_PROGRAM` value exported into termstage-created tmux sessions.
    pub(crate) const TERMINAL_PROGRAM: &str = "termstage";
    /// tmux history limit for termstage-created sessions.
    pub(crate) const HISTORY_LIMIT: &str = "100000";
    /// Color-disabling environment variables removed before spawning tmux commands.
    pub(crate) const DISABLE_COLOR_ENV_KEYS: [&str; 2] = ["NO_COLOR", "ANSI_COLORS_DISABLED"];
}

/// rmux backend process defaults.
#[cfg(feature = "backend-rmux")]
pub(crate) mod rmux_backend {
    use super::Duration;

    /// Default SDK operation timeout for daemon calls.
    pub(crate) const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
    /// `COLORTERM` value exported into termstage-created rmux sessions.
    pub(crate) const TERMINAL_COLOR_MODE: &str = "truecolor";
    /// `TERM_PROGRAM` value exported into termstage-created rmux sessions.
    pub(crate) const TERMINAL_PROGRAM: &str = "termstage";
}
