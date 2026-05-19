//! Local-only security validation for the browser terminal server.
//!
//! These types validate trust-boundary values before HTTP or WebSocket handlers
//! allocate runtime sessions.

use std::{
    fmt::{self, Debug, Formatter},
    net::{IpAddr, SocketAddr},
};

use thiserror::Error;
use url::Url;

use crate::protocol::AccessToken;

/// Security validation failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SecurityError {
    /// The host header did not match the selected loopback host and port.
    #[error("forbidden")]
    InvalidHost,
    /// The origin header was missing or mismatched.
    #[error("forbidden")]
    InvalidOrigin,
    /// The peer socket address was not loopback.
    #[error("forbidden")]
    InvalidPeer,
    /// The supplied token did not match the server token.
    #[error("forbidden")]
    InvalidToken,
}

/// Selected loopback bind target that requests must match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopbackBind {
    host: IpAddr,
    port: u16,
}

impl LoopbackBind {
    /// Creates a loopback bind target.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::InvalidPeer`] when `host` is not a loopback IP.
    pub fn new(host: IpAddr, port: u16) -> Result<Self, SecurityError> {
        if host.is_loopback() {
            Ok(Self { host, port })
        } else {
            Err(SecurityError::InvalidPeer)
        }
    }

    /// Returns the selected port.
    #[must_use]
    pub const fn port(self) -> u16 {
        self.port
    }

    fn host_matches(self, host: &str) -> bool {
        match self.host {
            IpAddr::V4(ip) => {
                host == ip.to_string()
                    || (ip.is_loopback() && host.eq_ignore_ascii_case("localhost"))
            }
            IpAddr::V6(ip) => host == format!("[{ip}]") || host == ip.to_string(),
        }
    }
}

/// Validated HTTP Host header.
#[derive(Clone, PartialEq, Eq)]
pub struct AllowedHost(String);

impl AllowedHost {
    /// Validates a Host header against the selected bind target.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::InvalidHost`] when the host or port does not
    /// exactly match the loopback bind target.
    pub fn validate(value: &str, bind: LoopbackBind) -> Result<Self, SecurityError> {
        let (host, port) = split_host_port(value).ok_or(SecurityError::InvalidHost)?;
        if port == bind.port && bind.host_matches(host) {
            Ok(Self(value.to_owned()))
        } else {
            Err(SecurityError::InvalidHost)
        }
    }

    /// Returns the validated header value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Debug for AllowedHost {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("AllowedHost").field(&self.0).finish()
    }
}

/// Validated same-origin header.
#[derive(Clone, PartialEq, Eq)]
pub struct AllowedOrigin(String);

impl AllowedOrigin {
    /// Validates an Origin header against the selected bind target.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::InvalidOrigin`] when the origin is absent,
    /// malformed, non-HTTP(S), or does not match host and port.
    pub fn validate(value: &str, bind: LoopbackBind) -> Result<Self, SecurityError> {
        let origin = Url::parse(value).map_err(|_error| SecurityError::InvalidOrigin)?;
        if origin.scheme() != "http" {
            return Err(SecurityError::InvalidOrigin);
        }
        if origin.path() != "/" || origin.query().is_some() || origin.fragment().is_some() {
            return Err(SecurityError::InvalidOrigin);
        }
        let host = origin.host_str().ok_or(SecurityError::InvalidOrigin)?;
        let port = origin
            .port_or_known_default()
            .ok_or(SecurityError::InvalidOrigin)?;
        if port == bind.port && bind.host_matches(host) {
            Ok(Self(value.to_owned()))
        } else {
            Err(SecurityError::InvalidOrigin)
        }
    }

    /// Returns the validated origin.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Debug for AllowedOrigin {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("AllowedOrigin")
            .field(&self.0)
            .finish()
    }
}

/// Validated loopback peer address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerAddr(SocketAddr);

impl PeerAddr {
    /// Validates that a socket peer is loopback.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::InvalidPeer`] when the peer IP is not loopback.
    pub fn validate(value: SocketAddr) -> Result<Self, SecurityError> {
        if value.ip().is_loopback() {
            Ok(Self(value))
        } else {
            Err(SecurityError::InvalidPeer)
        }
    }

    /// Returns the socket address.
    #[must_use]
    pub const fn get(self) -> SocketAddr {
        self.0
    }
}

/// Validates an access token without leaking token contents.
///
/// # Errors
///
/// Returns [`SecurityError::InvalidToken`] when the tokens differ.
pub fn validate_access_token(
    expected: &AccessToken,
    supplied: &AccessToken,
) -> Result<(), SecurityError> {
    if expected.constant_time_eq(supplied) {
        Ok(())
    } else {
        Err(SecurityError::InvalidToken)
    }
}

fn split_host_port(value: &str) -> Option<(&str, u16)> {
    if value.is_empty() || value.contains('/') {
        return None;
    }
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, port) = rest.split_once("]:")?;
        (host, port)
    } else {
        value.rsplit_once(':')?
    };
    let port = port.parse().ok()?;
    Some((host, port))
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    #[test]
    fn test_should_accept_exact_loopback_host() -> anyhow::Result<()> {
        let bind = LoopbackBind::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49200)?;
        let host = AllowedHost::validate("127.0.0.1:49200", bind)?;
        assert_eq!(host.as_str(), "127.0.0.1:49200");
        assert!(AllowedHost::validate("localhost:49200", bind).is_ok());
        Ok(())
    }

    #[test]
    fn test_should_reject_invalid_host() -> anyhow::Result<()> {
        let bind = LoopbackBind::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49200)?;
        assert!(matches!(
            AllowedHost::validate("127.0.0.1:49201", bind),
            Err(SecurityError::InvalidHost)
        ));
        assert!(matches!(
            AllowedHost::validate("example.com:49200", bind),
            Err(SecurityError::InvalidHost)
        ));
        Ok(())
    }

    #[test]
    fn test_should_accept_same_origin() -> anyhow::Result<()> {
        let bind = LoopbackBind::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49200)?;
        let origin = AllowedOrigin::validate("http://127.0.0.1:49200", bind)?;
        assert_eq!(origin.as_str(), "http://127.0.0.1:49200");
        Ok(())
    }

    #[test]
    fn test_should_reject_origin_mismatch() -> anyhow::Result<()> {
        let bind = LoopbackBind::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49200)?;
        assert!(matches!(
            AllowedOrigin::validate("http://127.0.0.1:49201", bind),
            Err(SecurityError::InvalidOrigin)
        ));
        assert!(matches!(
            AllowedOrigin::validate("https://127.0.0.1:49200", bind),
            Err(SecurityError::InvalidOrigin)
        ));
        assert!(matches!(
            AllowedOrigin::validate("file://127.0.0.1:49200", bind),
            Err(SecurityError::InvalidOrigin)
        ));
        Ok(())
    }

    #[test]
    fn test_should_validate_peer_loopback() -> anyhow::Result<()> {
        let local = SocketAddr::from((Ipv4Addr::LOCALHOST, 5000));
        let peer = PeerAddr::validate(local)?;
        assert_eq!(peer.get(), local);
        let remote = SocketAddr::from(([192, 0, 2, 1], 5000));
        assert!(matches!(
            PeerAddr::validate(remote),
            Err(SecurityError::InvalidPeer)
        ));
        Ok(())
    }

    #[test]
    fn test_should_validate_ipv6_loopback_host() -> anyhow::Result<()> {
        let bind = LoopbackBind::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 49200)?;
        assert!(AllowedHost::validate("[::1]:49200", bind).is_ok());
        Ok(())
    }

    #[test]
    fn test_should_compare_tokens_without_leaking() {
        let expected = AccessToken::from_bytes([1; 32]);
        let supplied = AccessToken::from_bytes([1; 32]);
        let wrong = AccessToken::from_bytes([2; 32]);
        assert!(validate_access_token(&expected, &supplied).is_ok());
        assert!(matches!(
            validate_access_token(&expected, &wrong),
            Err(SecurityError::InvalidToken)
        ));
        assert_eq!(format!("{:?}", SecurityError::InvalidToken), "InvalidToken");
    }
}
