/// Errors originating from the MCP server layer.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// Invalid input from the MCP client.
    #[error("Invalid input: {message} (code: {code})")]
    InvalidInput {
        code: String,
        message: String,
        suggestion: String,
    },

    /// The requested repository was not found.
    #[error("Repository not found: {0}")]
    RepoNotFound(String),

    /// Graph query failed.
    #[error("Graph error: {0}")]
    Graph(#[from] seshat_graph::GraphError),

    /// Transport or protocol error.
    #[error("Transport error: {0}")]
    Transport(String),
}
