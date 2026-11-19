//! Per-registry record of MCP server `instructions`, for the system prompt.
//!
//! Each background connection records its server's capped instructions here.
//! An agent snapshots the record once, at its first tool lock; servers that
//! connect after that add nothing to that session's prompt.

use crate::mcp::McpManager;
use crate::tool::Registry;
use tokio::sync::RwLock;

impl Registry {
    /// Record (or clear, with `None`) one server's initialize instructions.
    pub fn set_mcp_server_instructions(&self, server: &str, instructions: Option<String>) {
        let mut index = self.mcp_policy.write().unwrap_or_else(|e| e.into_inner());
        match instructions {
            Some(text) => index.instructions.insert(server.to_string(), text),
            None => index.instructions.remove(server),
        };
    }

    /// Record what a just-connected server sent in its initialize result.
    pub(crate) async fn record_mcp_instructions(&self, manager: &RwLock<McpManager>, server: &str) {
        let instructions = manager.read().await.server_instructions(server).await;
        self.set_mcp_server_instructions(server, instructions);
    }

    /// `(server, instructions)` for every connected server that sent some,
    /// ordered by server name.
    pub fn mcp_server_instructions(&self) -> Vec<(String, String)> {
        self.mcp_policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .instructions
            .iter()
            .map(|(server, text)| (server.clone(), text.clone()))
            .collect()
    }
}
