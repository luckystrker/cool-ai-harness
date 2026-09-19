//! One in-process App Protocol connection per browser client.
//!
//! The facade never re-implements command dispatch. Each browser identity
//! (cookie `cool_client` or `x-cool-client` header) is mapped to an
//! `AppServer::serve_io` task driven through a duplex pipe, so the HTTP layer
//! sees exactly the same request/response and live-event semantics as stdio,
//! TUI and ACP clients. Keeping the connection alive is what lets a run's
//! events reach the browser after the HTTP command request has returned.

use std::collections::HashMap;
use std::fmt;
use std::io;

use cool_app_server::{AppClient, ClientError};
use cool_protocol::ProtocolError;
use tokio::sync::Mutex;

use cool_app_server::AppServer;

/// Large enough for a full `event_page_limit` of max-size frames.
const CONNECTION_BUFFER: usize = 4 * 1024 * 1024;

/// Upper bound on live browser connections. A forged stream of client ids must
/// not be able to spawn unbounded `serve_io` tasks; new identities are refused
/// (503) once the pool is full instead of evicting an active run.
const MAX_CONNECTIONS: usize = 128;

pub(crate) struct ConnectionPool {
    server: AppServer,
    connections: Mutex<HashMap<String, AppClient>>,
}

#[derive(Debug)]
pub(crate) enum ConnectionError {
    Transport(String),
    Protocol(ProtocolError),
    LimitReached,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => write!(formatter, "connection transport error: {message}"),
            Self::Protocol(error) => {
                write!(
                    formatter,
                    "connection initialize failed: {}",
                    error.cool_code
                )
            }
            Self::LimitReached => write!(
                formatter,
                "the browser connection pool is full ({MAX_CONNECTIONS} live connections)"
            ),
        }
    }
}

impl std::error::Error for ConnectionError {}

impl From<ClientError> for ConnectionError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::Protocol(protocol) => Self::Protocol(protocol),
            other => Self::Transport(other.to_string()),
        }
    }
}

impl ConnectionPool {
    pub(crate) fn new(server: AppServer) -> Self {
        Self {
            server,
            connections: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the live connection for `id`, creating and initializing it on
    /// first use. Initialization runs outside the pool lock so a slow handshake
    /// cannot serialize every other identity; a racing duplicate is discarded
    /// before it owns any run, so dropping it is safe.
    pub(crate) async fn get(&self, id: &str) -> Result<AppClient, ConnectionError> {
        {
            let connections = self.connections.lock().await;
            if let Some(client) = connections.get(id) {
                return Ok(client.clone());
            }
            if connections.len() >= MAX_CONNECTIONS {
                return Err(ConnectionError::LimitReached);
            }
        }
        let (client_io, server_io) = tokio::io::duplex(CONNECTION_BUFFER);
        let (reader, writer) = tokio::io::split(client_io);
        let server = self.server.clone();
        tokio::spawn(async move {
            let _ = server.serve_io(server_io).await;
        });
        let client = AppClient::connect(reader, writer)
            .map_err(|error: io::Error| ConnectionError::Transport(error.to_string()))?;
        client
            .initialize("cool-web", env!("CARGO_PKG_VERSION"))
            .await?;
        let mut connections = self.connections.lock().await;
        if let Some(existing) = connections.get(id) {
            return Ok(existing.clone());
        }
        if connections.len() >= MAX_CONNECTIONS {
            return Err(ConnectionError::LimitReached);
        }
        connections.insert(id.to_owned(), client.clone());
        Ok(client)
    }

    /// The server-configured event pagination ceiling, so the SSE handler can
    /// never request a limit the runtime rejects.
    pub(crate) fn max_event_page_limit(&self) -> u16 {
        self.server.config().event_page_limit
    }
}
