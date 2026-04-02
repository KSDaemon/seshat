//! MCP server implementation using `rmcp`.
//!
//! `McpServer` is the main entry point. It registers tools with rmcp,
//! starts the requested transports, and handles graceful shutdown.

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_router,
};
use seshat_core::ServerConfig;

/// Request parameters for the `ping` diagnostic tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PingRequest {
    /// Optional message to echo back. Defaults to "pong" if omitted.
    #[schemars(description = "Optional message to echo back")]
    pub message: Option<String>,
}

/// The Seshat MCP server.
///
/// Registers tools via `rmcp`'s `#[tool_router]` macro and implements
/// `ServerHandler` for protocol compliance. Currently exposes a single
/// diagnostic `ping` tool; real tools will replace it in later stories.
#[derive(Debug, Clone)]
pub struct McpServer {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    #[allow(dead_code)]
    config: ServerConfig,
}

impl McpServer {
    /// Create a new `McpServer` with the given configuration.
    pub fn new(config: ServerConfig) -> Self {
        Self {
            tool_router: Self::tool_router(),
            config,
        }
    }
}

#[tool_router]
impl McpServer {
    /// Diagnostic tool that echoes a message.
    ///
    /// This is a temporary tool used to validate rmcp integration.
    /// It will be replaced by real tools (query_project_context, etc.)
    /// in later stories.
    #[tool(description = "Diagnostic ping — echoes a message to verify MCP connectivity")]
    fn ping(&self, Parameters(req): Parameters<PingRequest>) -> String {
        let msg = req.message.unwrap_or_else(|| "pong".to_owned());
        serde_json::json!({
            "status": "success",
            "tool": "ping",
            "data": { "message": msg }
        })
        .to_string()
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Seshat — convention-aware project intelligence for AI agents. \
                 Use query_project_context to understand the project, \
                 query_convention to look up coding conventions, \
                 and record_decision to capture team agreements.",
        )
    }
}

/// Start the MCP server on stdio transport.
///
/// This function blocks until the server is shut down (e.g., via Ctrl+C
/// or when the client closes the connection).
pub async fn start_stdio(config: ServerConfig) -> Result<(), crate::McpError> {
    let server = McpServer::new(config);

    tracing::info!("Starting MCP server on stdio transport");

    let transport = rmcp::transport::io::stdio();

    let service = server
        .serve(transport)
        .await
        .map_err(|e| crate::McpError::Transport(format!("{e}")))?;

    tracing::info!("MCP server running — waiting for client");

    // Wait for the service to complete (client disconnects or shutdown signal).
    service
        .waiting()
        .await
        .map_err(|e| crate::McpError::Transport(format!("{e}")))?;

    tracing::info!("MCP server stopped");

    Ok(())
}

/// Start the MCP server on stdio transport with an external shutdown signal.
///
/// Returns when either:
/// - The MCP client disconnects normally
/// - The `shutdown` future resolves (e.g., from Ctrl+C), after which the
///   server waits up to `drain_timeout` for the service to finish.
pub async fn start_stdio_with_shutdown(
    config: ServerConfig,
    shutdown: impl std::future::Future<Output = ()>,
    drain_timeout: std::time::Duration,
) -> Result<(), crate::McpError> {
    let server = McpServer::new(config);

    tracing::info!("Starting MCP server on stdio transport");

    let transport = rmcp::transport::io::stdio();

    let service = server
        .serve(transport)
        .await
        .map_err(|e| crate::McpError::Transport(format!("{e}")))?;

    tracing::info!("MCP server running — waiting for client");

    // Pin the shutdown future so we can poll it in select!.
    tokio::pin!(shutdown);

    tokio::select! {
        result = service.waiting() => {
            result.map_err(|e| crate::McpError::Transport(format!("{e}")))?;
            tracing::info!("MCP server stopped (client disconnected)");
        }
        _ = &mut shutdown => {
            tracing::info!("Shutdown signal received, waiting up to {drain_timeout:?} for drain");
            // Give active requests time to complete, then return.
            tokio::time::sleep(drain_timeout).await;
            tracing::info!("Drain period complete, shutting down");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_creates_with_default_config() {
        let server = McpServer::new(ServerConfig::default());
        let info = server.get_info();
        // ServerInfo should have tools capability enabled.
        assert!(info.capabilities.tools.is_some());
    }

    #[test]
    fn ping_tool_returns_json_with_default_message() {
        let server = McpServer::new(ServerConfig::default());
        let result = server.ping(Parameters(PingRequest { message: None }));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "ping");
        assert_eq!(parsed["data"]["message"], "pong");
    }

    #[test]
    fn ping_tool_echoes_custom_message() {
        let server = McpServer::new(ServerConfig::default());
        let result = server.ping(Parameters(PingRequest {
            message: Some("hello".to_owned()),
        }));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["data"]["message"], "hello");
    }
}
