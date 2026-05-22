//! Browser terminal protocol types.
//!
//! Binary WebSocket frames carry raw terminal bytes. Text frames carry these
//! validated control messages.

use std::{
    fmt::{self, Debug, Formatter},
    num::NonZeroU16,
    str::FromStr,
};

use getrandom::fill as fill_random;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use subtle::ConstantTimeEq;
use thiserror::Error;

const SESSION_NAME_MAX_BYTES: usize = 64;
const SAFE_MESSAGE_MAX_BYTES: usize = 512;
const ACCESS_TOKEN_BYTES: usize = 32;
const TERMINAL_COLS_MIN: u16 = 20;
const TERMINAL_COLS_MAX: u16 = 300;
const TERMINAL_ROWS_MIN: u16 = 5;
const TERMINAL_ROWS_MAX: u16 = 120;

/// Protocol validation failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// A session name is empty, too long, or contains disallowed bytes.
    #[error("invalid session name")]
    InvalidSessionName,
    /// A terminal column count is outside the supported range.
    #[error("terminal columns must be in 20..=300")]
    InvalidTerminalCols,
    /// A terminal row count is outside the supported range.
    #[error("terminal rows must be in 5..=120")]
    InvalidTerminalRows,
    /// A server-visible message is too long.
    #[error("safe message exceeds 512 bytes")]
    SafeMessageTooLong,
    /// Random token generation failed.
    #[error("failed to generate access token")]
    TokenGenerationFailed,
    /// A URL token was malformed.
    #[error("invalid access token")]
    InvalidAccessToken,
    /// A heartbeat sequence increment would overflow.
    #[error("heartbeat sequence overflow")]
    HeartbeatOverflow,
}

/// Valid tmux/session identifier for browser terminal mode.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionName(String);

impl SessionName {
    /// Creates a validated session name.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidSessionName`] when `value` is empty,
    /// longer than 64 bytes, or contains bytes outside `[A-Za-z0-9_.-]`.
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        let is_valid_len = !value.is_empty() && value.len() <= SESSION_NAME_MAX_BYTES;
        let is_valid_charset = value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
        if is_valid_len && is_valid_charset {
            Ok(Self(value))
        } else {
            Err(ProtocolError::InvalidSessionName)
        }
    }

    /// Returns the validated session name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Debug for SessionName {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("SessionName").field(&self.0).finish()
    }
}

impl FromStr for SessionName {
    type Err = ProtocolError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for SessionName {
    type Error = ProtocolError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for SessionName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SessionName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Valid terminal column count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct TerminalCols(NonZeroU16);

impl TerminalCols {
    /// Creates a validated terminal column count.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidTerminalCols`] when `value` is outside
    /// `20..=300`.
    pub fn new(value: u16) -> Result<Self, ProtocolError> {
        if (TERMINAL_COLS_MIN..=TERMINAL_COLS_MAX).contains(&value) {
            let cols = NonZeroU16::new(value).ok_or(ProtocolError::InvalidTerminalCols)?;
            Ok(Self(cols))
        } else {
            Err(ProtocolError::InvalidTerminalCols)
        }
    }

    /// Returns the column count as `u16`.
    #[must_use]
    pub fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u16> for TerminalCols {
    type Error = ProtocolError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for TerminalCols {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Valid terminal row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct TerminalRows(NonZeroU16);

impl TerminalRows {
    /// Creates a validated terminal row count.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidTerminalRows`] when `value` is outside
    /// `5..=120`.
    pub fn new(value: u16) -> Result<Self, ProtocolError> {
        if (TERMINAL_ROWS_MIN..=TERMINAL_ROWS_MAX).contains(&value) {
            let rows = NonZeroU16::new(value).ok_or(ProtocolError::InvalidTerminalRows)?;
            Ok(Self(rows))
        } else {
            Err(ProtocolError::InvalidTerminalRows)
        }
    }

    /// Returns the row count as `u16`.
    #[must_use]
    pub fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u16> for TerminalRows {
    type Error = ProtocolError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for TerminalRows {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Valid terminal dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalSize {
    /// Terminal columns.
    pub cols: TerminalCols,
    /// Terminal rows.
    pub rows: TerminalRows,
}

impl TerminalSize {
    /// Creates a validated terminal size.
    ///
    /// # Errors
    ///
    /// Returns a [`ProtocolError`] when either dimension is outside its valid
    /// range.
    pub fn new(cols: u16, rows: u16) -> Result<Self, ProtocolError> {
        Ok(Self {
            cols: TerminalCols::new(cols)?,
            rows: TerminalRows::new(rows)?,
        })
    }
}

/// Monotonic heartbeat sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HeartbeatSequence(u64);

impl HeartbeatSequence {
    /// Creates a heartbeat sequence value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric sequence.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the next sequence.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::HeartbeatOverflow`] at `u64::MAX`.
    pub fn next(self) -> Result<Self, ProtocolError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(ProtocolError::HeartbeatOverflow)
    }
}

/// Server-generated browser-visible message with bounded size.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SafeMessage(String);

impl SafeMessage {
    /// Creates a bounded safe message.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::SafeMessageTooLong`] when `value` exceeds 512
    /// bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        if value.len() <= SAFE_MESSAGE_MAX_BYTES {
            Ok(Self(value))
        } else {
            Err(ProtocolError::SafeMessageTooLong)
        }
    }

    /// Returns the safe message as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Creates a safe message from a trusted static string.
    #[must_use]
    pub fn from_static(value: &'static str) -> Self {
        Self(value.to_owned())
    }
}

impl Debug for SafeMessage {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("SafeMessage").field(&self.0).finish()
    }
}

impl<'de> Deserialize<'de> for SafeMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Per-server 256-bit access token.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken([u8; ACCESS_TOKEN_BYTES]);

impl AccessToken {
    /// Generates a fresh token from the operating system random source.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::TokenGenerationFailed`] if the OS random source
    /// cannot provide bytes.
    pub fn generate() -> Result<Self, ProtocolError> {
        let mut bytes = [0_u8; ACCESS_TOKEN_BYTES];
        fill_random(&mut bytes).map_err(|_error| ProtocolError::TokenGenerationFailed)?;
        Ok(Self(bytes))
    }

    /// Creates a token from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; ACCESS_TOKEN_BYTES]) -> Self {
        Self(bytes)
    }

    /// Returns true when two tokens match using constant-time comparison.
    #[must_use]
    pub fn constant_time_eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }

    /// Returns the token bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; ACCESS_TOKEN_BYTES] {
        &self.0
    }

    /// Encodes the token for explicit browser launch URLs.
    #[must_use]
    pub fn to_url_token(&self) -> String {
        let mut output = String::with_capacity(ACCESS_TOKEN_BYTES * 2);
        for byte in self.0 {
            push_hex_byte(byte, &mut output);
        }
        output
    }
}

impl Debug for AccessToken {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessToken([REDACTED])")
    }
}

impl FromStr for AccessToken {
    type Err = ProtocolError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != ACCESS_TOKEN_BYTES * 2 {
            return Err(ProtocolError::InvalidAccessToken);
        }
        let mut bytes = [0_u8; ACCESS_TOKEN_BYTES];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = decode_hex_nibble(*chunk.first().ok_or(ProtocolError::InvalidAccessToken)?)?;
            let low = decode_hex_nibble(*chunk.get(1).ok_or(ProtocolError::InvalidAccessToken)?)?;
            let Some(target) = bytes.get_mut(index) else {
                return Err(ProtocolError::InvalidAccessToken);
            };
            *target = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

/// Browser-to-server JSON control frame.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum ClientControlMessage {
    /// Resize the PTY to the validated dimensions.
    Resize {
        /// Terminal columns.
        cols: TerminalCols,
        /// Terminal rows.
        rows: TerminalRows,
    },
    /// Browser heartbeat with a monotonic sequence.
    Heartbeat {
        /// Heartbeat sequence.
        sequence: HeartbeatSequence,
    },
}

/// Stable warning reason codes emitted by the server.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WarningCode {
    /// The client output mailbox could not keep up.
    ClientBackpressure,
}

/// Stable error reason codes emitted by the server.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    /// A protocol message was malformed or invalid.
    Protocol,
    /// The runtime could not complete the requested operation.
    Runtime,
    /// The request failed a security check.
    Forbidden,
}

/// Server-to-browser JSON control frame.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum ServerControlMessage {
    /// Runtime session is ready.
    Ready {
        /// Active session name.
        session: SessionName,
    },
    /// The terminal process exited while the server kept the session open.
    ProcessExited {
        /// Browser-visible safe message.
        message: SafeMessage,
    },
    /// Non-fatal server warning.
    Warning {
        /// Stable warning code.
        code: WarningCode,
        /// Browser-visible safe message.
        message: SafeMessage,
    },
    /// Fatal server/runtime error.
    Error {
        /// Stable error code.
        code: ErrorCode,
        /// Browser-visible safe message.
        message: SafeMessage,
    },
}

fn push_hex_byte(byte: u8, output: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let high = usize::from(byte >> 4);
    let low = usize::from(byte & 0x0f);
    if let Some(value) = HEX.get(high) {
        output.push(char::from(*value));
    }
    if let Some(value) = HEX.get(low) {
        output.push(char::from(*value));
    }
}

fn decode_hex_nibble(byte: u8) -> Result<u8, ProtocolError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(ProtocolError::InvalidAccessToken),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_validate_session_names() -> anyhow::Result<()> {
        let name = SessionName::new("demo_1.ok-name")?;
        assert_eq!(name.as_str(), "demo_1.ok-name");
        assert!(matches!(
            SessionName::new(""),
            Err(ProtocolError::InvalidSessionName)
        ));
        assert!(matches!(
            SessionName::new("bad/name"),
            Err(ProtocolError::InvalidSessionName)
        ));
        assert!(matches!(
            SessionName::new("x".repeat(65)),
            Err(ProtocolError::InvalidSessionName)
        ));
        Ok(())
    }

    #[test]
    fn test_should_validate_terminal_dimensions() -> anyhow::Result<()> {
        let size = TerminalSize::new(80, 24)?;
        assert_eq!(size.cols.get(), 80);
        assert_eq!(size.rows.get(), 24);
        assert!(matches!(
            TerminalCols::new(19),
            Err(ProtocolError::InvalidTerminalCols)
        ));
        assert!(matches!(
            TerminalCols::new(301),
            Err(ProtocolError::InvalidTerminalCols)
        ));
        assert!(matches!(
            TerminalRows::new(4),
            Err(ProtocolError::InvalidTerminalRows)
        ));
        assert!(matches!(
            TerminalRows::new(121),
            Err(ProtocolError::InvalidTerminalRows)
        ));
        Ok(())
    }

    #[test]
    fn test_should_reject_unknown_control_fields() -> anyhow::Result<()> {
        let Err(error) = serde_json::from_str::<ClientControlMessage>(
            r#"{"type":"resize","cols":80,"rows":24,"extra":true}"#,
        ) else {
            anyhow::bail!("unknown fields unexpectedly decoded");
        };
        assert!(error.to_string().contains("unknown field"));
        Ok(())
    }

    #[test]
    fn test_should_round_trip_control_messages_as_camel_case() -> anyhow::Result<()> {
        let message = ClientControlMessage::Resize {
            cols: TerminalCols::new(100)?,
            rows: TerminalRows::new(30)?,
        };
        let json = serde_json::to_string(&message)?;
        assert_eq!(json, r#"{"type":"resize","cols":100,"rows":30}"#);
        let decoded: ClientControlMessage = serde_json::from_str(&json)?;
        assert_eq!(decoded, message);
        Ok(())
    }

    #[test]
    fn test_should_round_trip_process_exited_message_as_camel_case() -> anyhow::Result<()> {
        let message = ServerControlMessage::ProcessExited {
            message: SafeMessage::from_static("terminal process exited"),
        };
        let json = serde_json::to_string(&message)?;
        assert_eq!(
            json,
            r#"{"type":"processExited","message":"terminal process exited"}"#
        );
        let decoded: ServerControlMessage = serde_json::from_str(&json)?;
        assert_eq!(decoded, message);
        Ok(())
    }

    #[test]
    fn test_should_redact_access_token_debug() {
        let token = AccessToken::from_bytes([7; ACCESS_TOKEN_BYTES]);
        let debug = format!("{token:?}");
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains('7'));
    }

    #[test]
    fn test_should_compare_access_tokens_constant_time() {
        let token = AccessToken::from_bytes([1; ACCESS_TOKEN_BYTES]);
        let same = AccessToken::from_bytes([1; ACCESS_TOKEN_BYTES]);
        let different = AccessToken::from_bytes([2; ACCESS_TOKEN_BYTES]);
        assert!(token.constant_time_eq(&same));
        assert!(!token.constant_time_eq(&different));
    }

    #[test]
    fn test_should_encode_and_parse_url_token() -> anyhow::Result<()> {
        let token = AccessToken::from_bytes([0xab; ACCESS_TOKEN_BYTES]);
        let encoded = token.to_url_token();
        assert_eq!(encoded.len(), ACCESS_TOKEN_BYTES * 2);
        let parsed = AccessToken::from_str(&encoded)?;
        assert!(token.constant_time_eq(&parsed));
        assert!(matches!(
            AccessToken::from_str("not-a-token"),
            Err(ProtocolError::InvalidAccessToken)
        ));
        Ok(())
    }

    #[test]
    fn test_should_reject_overlong_safe_message() {
        assert!(matches!(
            SafeMessage::new("x".repeat(513)),
            Err(ProtocolError::SafeMessageTooLong)
        ));
    }
}
