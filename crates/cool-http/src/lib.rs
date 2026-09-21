//! M11 Web facade: a browser-facing HTTP/SSE projection of the App Protocol.
//!
//! The facade never owns agent, store or policy logic. It multiplexes one
//! in-process App Protocol connection per browser client (`serve_io` over a
//! duplex pipe), exposes a JSON-RPC command endpoint plus an SSE event stream,
//! and serves the production React bundle. Two deployment profiles are
//! enforced up front: `local` (loopback-only, optional token) and `server`
//! (explicit token + TLS/reverse-proxy boundary, fail-closed).

pub mod auth;
mod connection;
mod routes;

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::middleware;
use cool_app_server::AppServer;
use tokio::net::TcpListener;
use uuid::Uuid;

use crate::connection::ConnectionPool;

/// Deployment profile from the migration plan section 5.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeProfile {
    /// Default single-user path: bind loopback, optional token, no TLS.
    Local,
    /// Explicit opt-in VPS path: token mandatory, TLS or trusted proxy required.
    Server,
}

/// HTTP facade configuration.
#[derive(Clone, Debug)]
pub struct ServeOptions {
    pub profile: ServeProfile,
    pub bind: SocketAddr,
    /// Bearer token required for `/api/*` (all profiles when set).
    pub token: Option<String>,
    /// Public origin the browser uses, e.g. `https://cool.example.com`. Enables
    /// the strict Origin check and is required for `Server`.
    pub public_url: Option<String>,
    /// The operator terminates TLS at a trusted reverse proxy in front of Cool.
    pub trust_proxy: bool,
    /// The operator terminates TLS in-process (reserved: accepted as a boundary
    /// acknowledgement, TLS termination itself is a later deliverable).
    pub tls_terminated: bool,
    /// Directory with the production React build. `None` serves a diagnostic
    /// placeholder so a bare `cool serve` still answers health checks.
    pub assets: Option<PathBuf>,
    /// Allows `Local` to bind a non-loopback address. Requires a token.
    pub allow_remote: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            profile: ServeProfile::Local,
            bind: SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8000),
            token: None,
            public_url: None,
            trust_proxy: false,
            tls_terminated: false,
            assets: None,
            allow_remote: false,
        }
    }
}

/// Fail-closed configuration errors raised before the server binds.
#[derive(Debug, PartialEq, Eq)]
pub enum ServeError {
    ServerProfileRequiresToken,
    ServerProfileRequiresTlsBoundary,
    ServerProfileRequiresPublicUrl,
    WeakToken,
    RemoteBindRequiresExplicitOptIn,
    RemoteBindRequiresToken,
    InvalidPublicUrl,
}

impl fmt::Display for ServeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ServerProfileRequiresToken => {
                "the server profile requires an API token (--token or COOL_API_TOKEN)"
            }
            Self::ServerProfileRequiresTlsBoundary => {
                "the server profile requires --tls-terminated or --trusted-proxy"
            }
            Self::ServerProfileRequiresPublicUrl => {
                "the server profile requires --public-url so the Origin check is meaningful"
            }
            Self::WeakToken => "the API token must be at least 16 characters",
            Self::RemoteBindRequiresExplicitOptIn => {
                "the local profile refuses a non-loopback bind without --allow-remote"
            }
            Self::RemoteBindRequiresToken => "a non-loopback bind requires an API token",
            Self::InvalidPublicUrl => "--public-url must be an absolute http(s) origin",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ServeError {}

pub(crate) struct FacadeState {
    pub(crate) pool: ConnectionPool,
    pub(crate) auth: auth::AuthConfig,
    pub(crate) assets: Option<PathBuf>,
}

/// Validated HTTP facade. Cheap to build; call [`HttpFacade::serve`] to run.
pub struct HttpFacade {
    state: Arc<FacadeState>,
}

impl HttpFacade {
    pub fn new(server: AppServer, options: ServeOptions) -> Result<Self, ServeError> {
        let auth = auth::AuthConfig::from_options(&options)?;
        let state = Arc::new(FacadeState {
            pool: ConnectionPool::new(server),
            auth,
            assets: options.assets.clone(),
        });
        Ok(Self { state })
    }

    /// Builds the axum router. Exposed for tests and embedding.
    ///
    /// The router requires `ConnectInfo<SocketAddr>` (the loopback peer check);
    /// embedders must serve it with
    /// `into_make_service_with_connect_info::<SocketAddr>()`.
    pub fn router(&self) -> Router {
        routes::router(self.state.clone())
    }

    /// Runs the HTTP server until the listener errors. The caller owns the
    /// listener so tests can bind port 0.
    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        axum::serve(
            listener,
            self.router()
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    }
}

/// Shared client-identity extension injected by the identity middleware.
#[derive(Clone, Debug)]
pub(crate) struct ClientId {
    pub(crate) value: String,
    /// True when this request is the one that created the identity. SSE refuses
    /// to spawn a connection for a freshly minted, unauthenticated identity so
    /// a cross-site no-cors GET cannot exhaust the pool.
    pub(crate) minted: bool,
}

const CLIENT_HEADER: &str = "x-cool-client";
const CLIENT_COOKIE: &str = "cool_client";

/// Generates a connection identity and persists it as a cookie when new.
pub(crate) async fn client_identity(
    axum::extract::State(state): axum::extract::State<Arc<FacadeState>>,
    mut request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    // A valid header wins; a present-but-invalid header must not shadow a
    // valid cookie (that would silently rotate the identity mid-session).
    let provided = request
        .headers()
        .get(CLIENT_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .filter(|value| valid_client_id(value))
        .or_else(|| {
            auth::cookie_value(request.headers(), CLIENT_COOKIE)
                .filter(|value| valid_client_id(value))
        });
    let (id, is_new) = match provided {
        Some(id) => (id, false),
        None => (Uuid::new_v4().to_string(), true),
    };
    request.extensions_mut().insert(ClientId {
        value: id.clone(),
        minted: is_new,
    });
    let mut response = next.run(request).await;
    if is_new {
        let mut cookie = format!("{CLIENT_COOKIE}={id}; Path=/; HttpOnly; SameSite=Strict");
        if state.auth.secure_cookie {
            cookie.push_str("; Secure");
        }
        if let Ok(value) = axum::http::HeaderValue::from_str(&cookie) {
            response
                .headers_mut()
                .append(axum::http::header::SET_COOKIE, value);
        }
    }
    response
}

fn valid_client_id(value: &str) -> bool {
    (8..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Validates deployment-profile options without binding or building a server.
///
/// Callers should run this before any startup side effect (opening stores,
/// creating files, spawning loops) so a misconfigured `serve` exits before it
/// touches the data directory.
pub fn validate_options(options: &ServeOptions) -> Result<(), ServeError> {
    auth::AuthConfig::from_options(options).map(|_| ())
}

/// Stable human-readable profile name.
pub fn profile_name(profile: ServeProfile) -> &'static str {
    match profile {
        ServeProfile::Local => "local",
        ServeProfile::Server => "server",
    }
}
