//! Security validation for browser terminal server exposure modes.
//!
//! These types validate trust-boundary values before HTTP or WebSocket handlers
//! allocate runtime sessions.

use std::{
    fmt::{self, Debug, Formatter},
    net::{IpAddr, SocketAddr},
    str::FromStr,
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
    /// The public base URL was invalid for internet exposure mode.
    #[error("invalid public url")]
    InvalidPublicUrl,
    /// The supplied token did not match the server token.
    #[error("forbidden")]
    InvalidToken,
    /// The base path was not a single absolute path segment trail.
    #[error("invalid base path")]
    InvalidBasePath,
}

/// Request exposure policy selected at server startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExposurePolicy {
    /// Local loopback-only service.
    Local(LoopbackBind),
    /// Public service behind an HTTPS ingress or reverse proxy.
    Public(PublicBaseUrl),
}

impl ExposurePolicy {
    /// Creates a local loopback policy.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::InvalidPeer`] when `host` is not loopback.
    pub fn local(host: IpAddr, port: u16) -> Result<Self, SecurityError> {
        Ok(Self::Local(LoopbackBind::new(host, port)?))
    }
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

/// Browser-visible HTTPS base URL for public exposure mode.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicBaseUrl {
    url: Url,
    host: String,
    port: u16,
}

impl PublicBaseUrl {
    /// Creates a validated public base URL.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::InvalidPublicUrl`] when the URL is not HTTPS, has
    /// no host, contains credentials, query, or fragment, or has a non-root path.
    pub fn parse(value: &str) -> Result<Self, SecurityError> {
        let mut url = Url::parse(value).map_err(|_error| SecurityError::InvalidPublicUrl)?;
        if url.scheme() != "https" {
            return Err(SecurityError::InvalidPublicUrl);
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(SecurityError::InvalidPublicUrl);
        }
        let host = url
            .host_str()
            .ok_or(SecurityError::InvalidPublicUrl)?
            .to_ascii_lowercase();
        let port = url
            .port_or_known_default()
            .ok_or(SecurityError::InvalidPublicUrl)?;
        url.set_path("/");
        Ok(Self { url, host, port })
    }

    /// Returns the public URL as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    /// Returns the public URL with token and presentation settings.
    #[must_use]
    pub fn launch_url(&self, token: &AccessToken, font_size: u16, theme: &str) -> String {
        self.launch_url_with_base_path(token, font_size, theme, None)
    }

    /// Returns the public URL with token and presentation settings, mounted
    /// under an optional reverse-proxy base path.
    #[must_use]
    pub fn launch_url_with_base_path(
        &self,
        token: &AccessToken,
        font_size: u16,
        theme: &str,
        base_path: Option<&BasePath>,
    ) -> String {
        let mut url = self.url.clone();
        if let Some(prefix) = base_path {
            url.set_path(prefix.as_str());
        }
        url.query_pairs_mut()
            .append_pair("token", &token.to_url_token())
            .append_pair("fontSize", &font_size.to_string())
            .append_pair("theme", theme);
        url.to_string()
    }

    fn host_matches(&self, host: &str) -> bool {
        host.eq_ignore_ascii_case(&self.host)
    }

    fn port_matches(&self, port: Option<u16>) -> bool {
        port.unwrap_or(443) == self.port
    }
}

impl Debug for PublicBaseUrl {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublicBaseUrl")
            .field("url", &self.url.as_str())
            .finish_non_exhaustive()
    }
}

impl FromStr for PublicBaseUrl {
    type Err = SecurityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// Validated reverse-proxy base path prefix.
///
/// Used when the server is mounted under a path prefix by an upstream reverse
/// proxy (e.g. `/p/<sessionId>/`). The stored value always starts and ends with
/// `/`, never contains `..`, never contains empty segments, and only uses
/// RFC 3986 unreserved characters plus `-`, `_`, `.`, and `~`. The trailing
/// slash is stripped for use as an axum nest prefix via [`Self::nest_prefix`].
#[derive(Clone, PartialEq, Eq)]
pub struct BasePath {
    value: String,
}

impl BasePath {
    /// Maximum number of bytes the validated base path may occupy.
    pub const MAX_BYTES: usize = 256;

    /// Validates a base path string.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::InvalidBasePath`] when the value is empty,
    /// missing the leading or trailing slash, contains an empty segment, a
    /// `..` segment, or characters outside the path-safe set.
    pub fn parse(value: &str) -> Result<Self, SecurityError> {
        if value.is_empty() || value.len() > Self::MAX_BYTES {
            return Err(SecurityError::InvalidBasePath);
        }
        if !value.starts_with('/') || !value.ends_with('/') {
            return Err(SecurityError::InvalidBasePath);
        }
        let trimmed = value.trim_start_matches('/').trim_end_matches('/');
        if trimmed.is_empty() {
            return Err(SecurityError::InvalidBasePath);
        }
        for segment in trimmed.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(SecurityError::InvalidBasePath);
            }
            if !segment
                .bytes()
                .all(|byte| is_unreserved_path_byte(byte) || byte == b'%')
            {
                return Err(SecurityError::InvalidBasePath);
            }
        }
        Ok(Self {
            value: value.to_owned(),
        })
    }

    /// Returns the base path including the leading and trailing slashes.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Returns the base path with the trailing slash stripped, suitable for use
    /// as an axum `Router::nest` prefix (which forbids the trailing slash).
    #[must_use]
    pub fn nest_prefix(&self) -> &str {
        self.value.trim_end_matches('/')
    }
}

impl Debug for BasePath {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("BasePath")
            .field(&self.value)
            .finish()
    }
}

impl FromStr for BasePath {
    type Err = SecurityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

const fn is_unreserved_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/')
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

    /// Validates a Host header against the configured exposure policy.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::InvalidHost`] when the host header is invalid for
    /// the selected policy.
    pub fn validate_for_policy(
        value: &str,
        policy: &ExposurePolicy,
    ) -> Result<Self, SecurityError> {
        match policy {
            ExposurePolicy::Local(bind) => Self::validate(value, *bind),
            ExposurePolicy::Public(url) => {
                let (host, port) =
                    split_host_optional_port(value).ok_or(SecurityError::InvalidHost)?;
                if url.host_matches(host) && url.port_matches(port) {
                    Ok(Self(value.to_owned()))
                } else {
                    Err(SecurityError::InvalidHost)
                }
            }
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

    /// Validates an Origin header against the configured exposure policy.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::InvalidOrigin`] when the origin is invalid for
    /// the selected policy.
    pub fn validate_for_policy(
        value: &str,
        policy: &ExposurePolicy,
    ) -> Result<Self, SecurityError> {
        match policy {
            ExposurePolicy::Local(bind) => Self::validate(value, *bind),
            ExposurePolicy::Public(url) => {
                let origin = Url::parse(value).map_err(|_error| SecurityError::InvalidOrigin)?;
                if origin.scheme() != "https" {
                    return Err(SecurityError::InvalidOrigin);
                }
                if origin.path() != "/" || origin.query().is_some() || origin.fragment().is_some() {
                    return Err(SecurityError::InvalidOrigin);
                }
                let host = origin.host_str().ok_or(SecurityError::InvalidOrigin)?;
                if url.host_matches(host) && url.port_matches(origin.port_or_known_default()) {
                    Ok(Self(value.to_owned()))
                } else {
                    Err(SecurityError::InvalidOrigin)
                }
            }
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

/// Validates that a socket peer is allowed by the exposure policy.
///
/// # Errors
///
/// Returns [`SecurityError::InvalidPeer`] when local mode receives a non-loopback
/// peer.
pub fn validate_peer_for_policy(
    value: SocketAddr,
    policy: &ExposurePolicy,
) -> Result<PeerAddr, SecurityError> {
    match policy {
        ExposurePolicy::Local(_bind) => PeerAddr::validate(value),
        ExposurePolicy::Public(_url) => Ok(PeerAddr(value)),
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

fn split_host_optional_port(value: &str) -> Option<(&str, Option<u16>)> {
    if value.is_empty() || value.contains('/') {
        return None;
    }
    if let Some(rest) = value.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']')?;
        let port = if suffix.is_empty() {
            None
        } else {
            let port = suffix.strip_prefix(':')?.parse().ok()?;
            Some(port)
        };
        return Some((host, port));
    }
    if let Some((host, port)) = value.rsplit_once(':') {
        if host.contains(':') {
            return Some((value, None));
        }
        return Some((host, Some(port.parse().ok()?)));
    }
    Some((value, None))
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
    fn test_should_validate_public_base_url() -> anyhow::Result<()> {
        let url = PublicBaseUrl::parse("https://term.example.com/")?;
        assert_eq!(url.as_str(), "https://term.example.com/");
        assert!(matches!(
            PublicBaseUrl::parse("http://term.example.com/"),
            Err(SecurityError::InvalidPublicUrl)
        ));
        assert!(matches!(
            PublicBaseUrl::parse("https://term.example.com/path"),
            Err(SecurityError::InvalidPublicUrl)
        ));
        assert!(matches!(
            PublicBaseUrl::parse("https://user@term.example.com/"),
            Err(SecurityError::InvalidPublicUrl)
        ));
        Ok(())
    }

    #[test]
    fn test_should_validate_public_host_and_origin() -> anyhow::Result<()> {
        let policy = ExposurePolicy::Public(PublicBaseUrl::parse("https://term.example.com/")?);
        assert!(AllowedHost::validate_for_policy("term.example.com", &policy).is_ok());
        assert!(AllowedHost::validate_for_policy("term.example.com:443", &policy).is_ok());
        assert!(AllowedOrigin::validate_for_policy("https://term.example.com", &policy).is_ok());
        assert!(matches!(
            AllowedHost::validate_for_policy("evil.example", &policy),
            Err(SecurityError::InvalidHost)
        ));
        assert!(matches!(
            AllowedOrigin::validate_for_policy("https://evil.example", &policy),
            Err(SecurityError::InvalidOrigin)
        ));
        assert!(matches!(
            AllowedOrigin::validate_for_policy("http://term.example.com", &policy),
            Err(SecurityError::InvalidOrigin)
        ));
        Ok(())
    }

    #[test]
    fn test_should_allow_public_non_loopback_peer() -> anyhow::Result<()> {
        let policy = ExposurePolicy::Public(PublicBaseUrl::parse("https://term.example.com/")?);
        let remote = SocketAddr::from(([192, 0, 2, 1], 5000));
        let peer = validate_peer_for_policy(remote, &policy)?;
        assert_eq!(peer.get(), remote);
        Ok(())
    }

    #[test]
    fn test_should_parse_valid_base_path() -> anyhow::Result<()> {
        let parsed = BasePath::parse("/p/abc123/")?;
        assert_eq!(parsed.as_str(), "/p/abc123/");
        assert_eq!(parsed.nest_prefix(), "/p/abc123");
        let multi = BasePath::parse("/coder/p/abc-123_v2/")?;
        assert_eq!(multi.as_str(), "/coder/p/abc-123_v2/");
        Ok(())
    }

    #[test]
    fn test_should_reject_invalid_base_path() {
        for value in [
            "",
            "/",
            "p/abc/",
            "/p/abc",
            "/p//abc/",
            "/p/../abc/",
            "/p/./abc/",
            "/p/abc?x=1/",
            "/p/abc with space/",
        ] {
            assert!(
                matches!(BasePath::parse(value), Err(SecurityError::InvalidBasePath)),
                "expected {value:?} to be rejected"
            );
        }
    }

    #[test]
    fn test_should_render_public_launch_url_with_base_path() -> anyhow::Result<()> {
        let url = PublicBaseUrl::parse("https://term.example.com/")?;
        let token = AccessToken::from_bytes([1; 32]);
        let base = BasePath::parse("/p/sess-1/")?;
        let rendered = url.launch_url_with_base_path(&token, 24, "high-contrast", Some(&base));
        assert!(
            rendered.starts_with("https://term.example.com/p/sess-1/?"),
            "{rendered}"
        );
        assert!(rendered.contains("token="));
        assert!(rendered.contains("fontSize=24"));
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
