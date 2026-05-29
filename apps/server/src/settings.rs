//! Named server settings grouped by subsystem ownership.

use std::{
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};

/// CLI-only defaults and display constants.
pub(crate) mod cli_defaults {
    /// Maximum accepted byte length for the environment variable name that
    /// carries an access token.
    pub(crate) const TOKEN_ENV_MAX_BYTES: usize = 128;
    /// Human-readable columns printed by `termstage session list`.
    pub(crate) const SESSION_LIST_HEADERS: [&str; 3] = ["SESSION_ID", "BACKEND", "DISPLAY_NAME"];
}

/// Browser presentation defaults shared by CLI validation and server launch URLs.
pub(crate) mod browser_presentation {
    /// Default browser terminal font size in CSS pixels.
    pub(crate) const DEFAULT_FONT_SIZE: u16 = 24;
    /// Smallest accepted browser terminal font size.
    pub(crate) const FONT_SIZE_MIN: u16 = 12;
    /// Largest accepted browser terminal font size.
    pub(crate) const FONT_SIZE_MAX: u16 = 96;
}

/// HTTP server and WebSocket transport limits.
pub(crate) mod http_server {
    use super::{IpAddr, Ipv4Addr};

    /// Default bind host for local browser terminal servers.
    pub(crate) const DEFAULT_BIND_HOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    /// Maximum WebSocket frame size accepted from browser clients.
    pub(crate) const WEBSOCKET_MAX_FRAME_BYTES: usize = 16 * 1024;
    /// Maximum WebSocket message size accepted from browser clients.
    pub(crate) const WEBSOCKET_MAX_MESSAGE_BYTES: usize = 64 * 1024;
    /// Maximum HTTP JSON request body size for semantic API calls.
    pub(crate) const API_REQUEST_BODY_LIMIT_BYTES: usize = 8 * 1024;
}

/// Browser WebSocket lifecycle timers and close reasons.
pub(crate) mod browser_socket {
    use super::Duration;

    /// Browser clients must send heartbeat frames within this period.
    pub(crate) const CLIENT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);
    /// Interval used by the server to check browser heartbeat freshness.
    pub(crate) const CLIENT_HEARTBEAT_CHECK_INTERVAL: Duration = Duration::from_secs(10);
    /// Interval used by the server to send WebSocket ping frames.
    pub(crate) const SERVER_PING_INTERVAL: Duration = Duration::from_secs(25);
    /// Static WebSocket ping payload.
    pub(crate) const SERVER_PING_PAYLOAD: &[u8] = b"termstage";
    /// Close reason sent when the backend or runtime session has ended.
    pub(crate) const CLOSE_REASON_SESSION_ENDED: &str = "session ended";
    /// Close reason sent when termstage is shutting down the server.
    #[cfg(test)]
    pub(crate) const CLOSE_REASON_SERVER_SHUTDOWN: &str = "server shutting down";
    /// Close reason used when the browser disconnects first.
    #[cfg(test)]
    pub(crate) const CLOSE_REASON_CLIENT_DISCONNECTED: &str = "client disconnected";
    /// Close reason sent when another controller replaces this browser.
    #[cfg(test)]
    pub(crate) const CLOSE_REASON_CONTROLLER_REPLACED: &str = "controller replaced";
    /// Close reason sent when the runtime path reports an unrecoverable error.
    #[cfg(test)]
    pub(crate) const CLOSE_REASON_RUNTIME_ERROR: &str = "runtime error";
}

/// Backend gateway timing for tmux/rmux/future backend-owned sessions.
pub(crate) mod backend_gateway {
    use super::Duration;

    /// Default operation-lock lease time for backend-owned terminal sessions.
    pub(crate) const OPERATION_LOCK_LEASE_TTL: Duration = Duration::from_secs(90);
    /// Poll interval used by CLI supervisors while waiting for backend session exit.
    pub(crate) const SESSION_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(250);
    /// Idle interval for reading backend screen snapshots for browser projection.
    pub(crate) const SCREEN_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
    /// Fast interval used shortly after browser input, resize, viewport, or lease changes.
    pub(crate) const SCREEN_ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(50);
    /// Duration of the fast polling window after browser-side activity.
    pub(crate) const SCREEN_ACTIVE_POLL_WINDOW: Duration = Duration::from_secs(2);
}

/// Runtime tunnel transport settings.
#[cfg(test)]
pub(crate) mod runtime_tunnel {
    /// Capacity for browser/runtime tunnel MPSC channels.
    pub(crate) const CHANNEL_CAPACITY: usize = 32;
}

/// Semantic API validation limits and polling timers.
pub(crate) mod semantic_api {
    use super::Duration;

    /// Maximum bytes accepted by `write-text` and `run-command`.
    pub(crate) const TEXT_MAX_BYTES: usize = 4096;
    /// Maximum bytes accepted by `press-key` token names.
    pub(crate) const KEY_MAX_BYTES: usize = 32;
    /// Maximum scroll amount accepted by `scroll`.
    pub(crate) const SCROLL_MAX_AMOUNT: u16 = 100;
    /// Maximum bytes accepted by `run-command.waitFor`.
    pub(crate) const WAIT_TEXT_MAX_BYTES: usize = 4096;
    /// Default `run-command` wait timeout in milliseconds.
    pub(crate) const WAIT_TIMEOUT_DEFAULT_MS: u64 = 5_000;
    /// Maximum `run-command` wait timeout in milliseconds.
    pub(crate) const WAIT_TIMEOUT_MAX_MS: u64 = 60_000;
    /// Poll interval while waiting for expected screen text.
    pub(crate) const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
}

/// CLI semantic API client parsing limits.
pub(crate) mod api_client {
    /// Maximum response size read by the CLI semantic API client.
    pub(crate) const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
    /// HTTP header delimiter used by the minimal CLI API client.
    pub(crate) const HTTP_HEADER_SEPARATOR: &[u8; 4] = b"\r\n\r\n";
    /// HTTP line delimiter used by the minimal CLI API client.
    pub(crate) const HTTP_LINE_SEPARATOR: &[u8; 2] = b"\r\n";
}

/// Termstage-owned backend session naming rules.
pub(crate) mod session_names {
    /// Prefix applied to backend sessions created by termstage.
    pub(crate) const TERMSTAGE_SESSION_PREFIX: &str = "TerminalUse-";
    /// Legacy prefix used by older termstage tmux-created sessions.
    pub(crate) const LEGACY_TERMSTAGE_TMUX_SESSION_PREFIX: &str = "ts-";
}
