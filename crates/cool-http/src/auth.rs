//! Authentication, origin/CSRF and loopback policy for the HTTP facade.
//!
//! Two profiles exist. `Local` is loopback-only by default and treats the OS
//! user boundary as the credential, with an optional shared token. `Server` is
//! an explicit opt-in that fails closed unless a token, a public origin and a
//! TLS/reverse-proxy boundary are all configured.

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::{FacadeState, ServeError, ServeOptions, ServeProfile};

const TOKEN_COOKIE: &str = "cool_token";
const MIN_TOKEN_LEN: usize = 16;

#[derive(Debug)]
pub(crate) struct AuthConfig {
    token: Option<String>,
    public_origin: Option<Origin>,
    /// Reject non-loopback peers (local default).
    pub(crate) require_loopback_peer: bool,
    /// Reject mutating requests that carry no Origin (server profile).
    require_origin: bool,
    pub(crate) secure_cookie: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
}

impl AuthConfig {
    pub(crate) fn from_options(options: &ServeOptions) -> Result<Self, ServeError> {
        if let Some(token) = &options.token
            && token.len() < MIN_TOKEN_LEN
        {
            return Err(ServeError::WeakToken);
        }
        let public_origin = options
            .public_url
            .as_deref()
            .map(Origin::parse)
            .transpose()?;
        let loopback_bind = options.bind.ip().is_loopback();
        match options.profile {
            ServeProfile::Local => {
                if !loopback_bind {
                    if !options.allow_remote {
                        return Err(ServeError::RemoteBindRequiresExplicitOptIn);
                    }
                    if options.token.is_none() {
                        return Err(ServeError::RemoteBindRequiresToken);
                    }
                }
                Ok(Self {
                    token: options.token.clone(),
                    public_origin,
                    require_loopback_peer: !options.allow_remote,
                    require_origin: false,
                    secure_cookie: false,
                })
            }
            ServeProfile::Server => {
                if options.token.is_none() {
                    return Err(ServeError::ServerProfileRequiresToken);
                }
                if !options.tls_terminated && !options.trust_proxy {
                    return Err(ServeError::ServerProfileRequiresTlsBoundary);
                }
                if public_origin.is_none() {
                    return Err(ServeError::ServerProfileRequiresPublicUrl);
                }
                Ok(Self {
                    token: options.token.clone(),
                    public_origin,
                    require_loopback_peer: false,
                    require_origin: true,
                    secure_cookie: true,
                })
            }
        }
    }

    /// True when a shared token is required, i.e. an anonymous request has
    /// already been authenticated by the time it reaches a handler.
    pub(crate) fn token_configured(&self) -> bool {
        self.token.is_some()
    }

    /// Verifies the bearer/query/cookie token when one is configured.
    pub(crate) fn authorized(&self, headers: &HeaderMap, uri: &Uri) -> bool {
        let Some(expected) = &self.token else {
            return true;
        };
        let candidate = bearer_token(headers)
            .or_else(|| query_token(uri))
            .or_else(|| cookie_value(headers, TOKEN_COOKIE));
        candidate.is_some_and(|value| constant_time_eq(value.as_bytes(), expected.as_bytes()))
    }

    /// Cross-site request forgery defense. Whenever an Origin header is
    /// present it must match the configured public/loopback origin, for reads
    /// as well as mutations — otherwise a cross-site page could still mint
    /// protocol connections via `GET /api/events`. Mutations additionally
    /// require an Origin in the `Server` profile.
    ///
    /// A *present but unparseable* Origin (`null`, empty, an opaque sandboxed
    /// origin) is never treated as an absent Origin: opaque origins are
    /// browser-reachable and must not bypass the check.
    pub(crate) fn origin_allowed(&self, method: &Method, headers: &HeaderMap) -> bool {
        let mut origins = headers.get_all(axum::http::header::ORIGIN).iter();
        let first = origins.next();
        if origins.next().is_some() {
            // A browser sends exactly one Origin; duplicates are hostile.
            return false;
        }
        match first {
            // A missing Origin is acceptable for non-browser clients, except
            // for mutations in the server profile where the browser boundary
            // is mandatory.
            None => !(self.require_origin && is_mutation(method)),
            Some(value) => match value.to_str().ok().map(Origin::parse) {
                Some(Ok(origin)) => self.origin_matches(&origin),
                _ => false,
            },
        }
    }

    fn origin_matches(&self, origin: &Origin) -> bool {
        match &self.public_origin {
            Some(expected) => origin == expected,
            None => origin.is_loopback(),
        }
    }
}

fn is_mutation(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

impl Origin {
    fn parse(value: &str) -> Result<Self, ServeError> {
        let uri = Uri::from_str(value).map_err(|_| ServeError::InvalidPublicUrl)?;
        let scheme = uri.scheme_str().ok_or(ServeError::InvalidPublicUrl)?;
        if scheme != "http" && scheme != "https" {
            return Err(ServeError::InvalidPublicUrl);
        }
        // `http::Uri::host()` ignores userinfo, so `http://evil.com@localhost/`
        // would otherwise read as `localhost`; a browser never puts userinfo in
        // an Origin, so reject it outright.
        if uri
            .authority()
            .is_some_and(|authority| authority.as_str().contains('@'))
        {
            return Err(ServeError::InvalidPublicUrl);
        }
        let host = uri
            .host()
            .filter(|host| !host.is_empty())
            .ok_or(ServeError::InvalidPublicUrl)?;
        // A path, query or fragment in the public URL is not a bare origin.
        if !matches!(uri.path(), "" | "/") || uri.query().is_some() {
            return Err(ServeError::InvalidPublicUrl);
        }
        Ok(Self {
            scheme: scheme.to_owned(),
            host: host.to_ascii_lowercase(),
            port: uri.port_u16(),
        })
    }

    fn is_loopback(&self) -> bool {
        if self.host == "localhost" {
            return true;
        }
        // `http::Uri` keeps IPv6 authorities bracketed (`[::1]`).
        let host = self
            .host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(&self.host);
        if let Ok(address) = host.parse::<std::net::IpAddr>() {
            return address.is_loopback();
        }
        false
    }
}

pub(crate) async fn authorize(
    State(state): State<Arc<FacadeState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if request.uri().path() == "/api/health" {
        return next.run(request).await;
    }
    if state.auth.require_loopback_peer && !peer.ip().is_loopback() {
        return deny(StatusCode::FORBIDDEN, "loopback_required");
    }
    if !state.auth.authorized(request.headers(), request.uri()) {
        return deny(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    if !state
        .auth
        .origin_allowed(request.method(), request.headers())
    {
        return deny(StatusCode::FORBIDDEN, "origin_not_allowed");
    }
    next.run(request).await
}

fn deny(status: StatusCode, code: &str) -> Response {
    (
        status,
        axum::Json(json!({
            "coolCode": code,
            "message": "the request was rejected by the HTTP facade policy",
            "retryable": false
        })),
    )
        .into_response()
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    Some(token.trim().to_owned())
}

fn query_token(uri: &Uri) -> Option<String> {
    let query = uri.query()?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "token")
        .map(|(_, value)| value.into_owned())
}

pub(crate) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookies = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then(|| value.trim().to_owned())
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn options(profile: ServeProfile) -> ServeOptions {
        ServeOptions {
            profile,
            bind: SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            ..ServeOptions::default()
        }
    }

    #[test]
    fn local_profile_defaults_to_loopback_without_token() {
        let config = AuthConfig::from_options(&options(ServeProfile::Local)).expect("config");
        assert!(config.require_loopback_peer);
        assert!(config.token.is_none());
        assert!(config.origin_allowed(&Method::POST, &HeaderMap::new()));
    }

    #[test]
    fn local_profile_rejects_remote_bind_without_opt_in() {
        let mut options = options(ServeProfile::Local);
        options.bind = SocketAddr::new("0.0.0.0".parse().unwrap(), 8000);
        assert_eq!(
            AuthConfig::from_options(&options).unwrap_err(),
            ServeError::RemoteBindRequiresExplicitOptIn
        );
        options.allow_remote = true;
        assert_eq!(
            AuthConfig::from_options(&options).unwrap_err(),
            ServeError::RemoteBindRequiresToken
        );
        options.token = Some("0123456789abcdef".to_owned());
        let config = AuthConfig::from_options(&options).expect("remote local config");
        assert!(!config.require_loopback_peer);
        assert!(config.token.is_some());
    }

    #[test]
    fn server_profile_is_fail_closed() {
        let mut options = options(ServeProfile::Server);
        assert_eq!(
            AuthConfig::from_options(&options).unwrap_err(),
            ServeError::ServerProfileRequiresToken
        );
        options.token = Some("0123456789abcdef".to_owned());
        assert_eq!(
            AuthConfig::from_options(&options).unwrap_err(),
            ServeError::ServerProfileRequiresTlsBoundary
        );
        options.trust_proxy = true;
        assert_eq!(
            AuthConfig::from_options(&options).unwrap_err(),
            ServeError::ServerProfileRequiresPublicUrl
        );
        options.public_url = Some("https://cool.example.com".to_owned());
        let config = AuthConfig::from_options(&options).expect("server config");
        assert!(config.secure_cookie);
        assert!(config.require_origin);
        assert!(!config.require_loopback_peer);
    }

    #[test]
    fn weak_tokens_and_bad_public_urls_are_rejected() {
        let mut options = options(ServeProfile::Local);
        options.token = Some("short".to_owned());
        assert_eq!(
            AuthConfig::from_options(&options).unwrap_err(),
            ServeError::WeakToken
        );
        options.token = None;
        options.public_url = Some("ftp://cool.example.com".to_owned());
        assert_eq!(
            AuthConfig::from_options(&options).unwrap_err(),
            ServeError::InvalidPublicUrl
        );
        options.public_url = Some("https://cool.example.com/path".to_owned());
        assert_eq!(
            AuthConfig::from_options(&options).unwrap_err(),
            ServeError::InvalidPublicUrl
        );
    }

    #[test]
    fn origin_policy_matches_exact_public_origin() {
        let mut options = options(ServeProfile::Server);
        options.token = Some("0123456789abcdef".to_owned());
        options.trust_proxy = true;
        options.public_url = Some("https://cool.example.com".to_owned());
        let config = AuthConfig::from_options(&options).expect("config");
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ORIGIN,
            "https://cool.example.com".parse().unwrap(),
        );
        assert!(config.origin_allowed(&Method::POST, &headers));
        headers.insert(
            axum::http::header::ORIGIN,
            "https://evil.example.com".parse().unwrap(),
        );
        assert!(!config.origin_allowed(&Method::POST, &headers));
        assert!(!config.origin_allowed(&Method::POST, &HeaderMap::new()));
        assert!(config.origin_allowed(&Method::GET, &HeaderMap::new()));
    }

    #[test]
    fn local_origin_policy_accepts_loopback_only() {
        let config = AuthConfig::from_options(&options(ServeProfile::Local)).expect("config");
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ORIGIN,
            "http://localhost:5173".parse().unwrap(),
        );
        assert!(config.origin_allowed(&Method::POST, &headers));
        headers.insert(
            axum::http::header::ORIGIN,
            "https://evil.example.com".parse().unwrap(),
        );
        assert!(!config.origin_allowed(&Method::POST, &headers));
    }

    #[test]
    fn opaque_and_duplicate_origins_are_rejected() {
        let config = AuthConfig::from_options(&options(ServeProfile::Local)).expect("config");
        for opaque in ["null", "", "about:blank", "data:text/plain,hi"] {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::ORIGIN,
                axum::http::HeaderValue::from_str(opaque).unwrap(),
            );
            assert!(
                !config.origin_allowed(&Method::POST, &headers),
                "opaque origin {opaque:?} must be rejected"
            );
        }
        let mut duplicate = HeaderMap::new();
        duplicate.append(
            axum::http::header::ORIGIN,
            "http://localhost:5173".parse().unwrap(),
        );
        duplicate.append(
            axum::http::header::ORIGIN,
            "https://evil.example.com".parse().unwrap(),
        );
        assert!(!config.origin_allowed(&Method::POST, &duplicate));
    }

    #[test]
    fn cross_site_reads_are_rejected_and_userinfo_origins_fail() {
        let config = AuthConfig::from_options(&options(ServeProfile::Local)).expect("config");
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ORIGIN,
            "https://evil.example.com".parse().unwrap(),
        );
        // A cross-site GET (e.g. EventSource) must not be able to mint a
        // protocol connection.
        assert!(!config.origin_allowed(&Method::GET, &headers));
        assert!(config.origin_allowed(&Method::GET, &HeaderMap::new()));
        assert!(Origin::parse("http://evil.com@localhost/").is_err());
        assert!(Origin::parse("http://localhost@evil.com/").is_err());
    }

    #[test]
    fn ipv6_loopback_origin_is_allowed() {
        let config = AuthConfig::from_options(&options(ServeProfile::Local)).expect("config");
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ORIGIN,
            "http://[::1]:5173".parse().unwrap(),
        );
        assert!(config.origin_allowed(&Method::POST, &headers));
    }

    #[test]
    fn token_is_checked_from_header_query_and_cookie() {
        let mut options = options(ServeProfile::Local);
        options.token = Some("0123456789abcdef".to_owned());
        let config = AuthConfig::from_options(&options).expect("config");
        let uri: Uri = "/api/rpc".parse().unwrap();
        assert!(!config.authorized(&HeaderMap::new(), &uri));
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer 0123456789abcdef".parse().unwrap(),
        );
        assert!(config.authorized(&headers, &uri));
        let query: Uri = "/api/rpc?token=0123456789abcdef".parse().unwrap();
        assert!(config.authorized(&HeaderMap::new(), &query));
        let mut cookie = HeaderMap::new();
        cookie.insert(
            axum::http::header::COOKIE,
            "cool_token=0123456789abcdef".parse().unwrap(),
        );
        assert!(config.authorized(&cookie, &uri));
    }
}
