//! ACP v1 adapter over the Rust trusted core.
//!
//! `cool acp` speaks newline-delimited JSON-RPC 2.0 on stdio. The adapter owns
//! no session state, approval state or tool execution: those stay in the
//! durable Rust runtime reached through the App Protocol client.

mod connection;
mod projection;

pub use connection::{
    ACP_PROTOCOL_VERSION, AcpError, AcpServer, DEFAULT_APPROVAL_TIMEOUT, MAX_JSON_DEPTH,
    MAX_MESSAGE_BYTES, prompt_text, stop_reason,
};
pub use projection::AcpProjection;

use std::io;
use std::path::PathBuf;

use cool_app_server::AppClient;

/// Runs the ACP stdio server against an already connected App Protocol client.
pub async fn run_stdio(client: AppClient, workspace: PathBuf) -> io::Result<()> {
    let server = std::sync::Arc::new(AcpServer::new(
        client,
        workspace,
        Box::new(tokio::io::stdout()),
    ));
    let stdin = tokio::io::stdin();
    server.serve_io(stdin).await
}
