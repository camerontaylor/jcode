//! Per-registry record of MCP server `instructions`, for the system prompt.
//!
//! Each background connection records its server's capped instructions here.
//! An agent snapshots the record once, at its first tool lock. If initial
//! connections are still in their handshake then, it waits for them, but never
//! past [`MCP_INSTRUCTIONS_GRACE`] after registration began. Servers that
//! connect after that, or fail, add nothing to that session's prompt.

use crate::mcp::McpManager;
use crate::tool::Registry;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// How long after MCP registration starts the first turn may wait for
/// initial handshakes. Local stdio servers finish well within it; a slow or
/// unreachable server delays the first request by at most this, once.
pub(crate) const MCP_INSTRUCTIONS_GRACE: Duration = Duration::from_millis(1000);

const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Default)]
pub(in crate::tool) struct Record {
    by_server: BTreeMap<String, String>,
    /// Initial connections whose handshake has not finished yet.
    pending: usize,
    deadline: Option<Instant>,
}

impl Registry {
    fn with_record<T>(&self, f: impl FnOnce(&mut Record) -> T) -> T {
        let mut index = self.mcp_policy.write().unwrap_or_else(|e| e.into_inner());
        f(&mut index.instructions)
    }

    /// Initial connections to `count` servers are starting now.
    pub(crate) fn begin_mcp_connects(&self, count: usize) {
        self.with_record(|record| {
            record.pending = count;
            record.deadline = Some(Instant::now() + MCP_INSTRUCTIONS_GRACE);
        });
    }

    /// One initial connection finished. Records its instructions on success
    /// and logs a failure. Returns whether it connected.
    pub(crate) async fn finish_mcp_connect(
        &self,
        manager: &RwLock<McpManager>,
        server: &str,
        result: anyhow::Result<()>,
    ) -> bool {
        let instructions = match &result {
            Ok(()) => manager.read().await.server_instructions(server).await,
            Err(_) => None,
        };
        self.set_mcp_server_instructions(server, instructions);
        self.with_record(|record| record.pending = record.pending.saturating_sub(1));
        match result {
            Ok(()) => {
                crate::logging::info(&format!("MCP: Connected to '{server}'"));
                true
            }
            Err(error) => {
                crate::logging::event_rate_limited(
                    crate::logging::LogLevel::Error,
                    &format!("mcp_register_failed:{server}"),
                    Duration::from_secs(60),
                    "MCP_REGISTER_FAILED",
                    vec![
                        ("server", server.to_string()),
                        ("error", format!("{error:#}")),
                    ],
                );
                false
            }
        }
    }

    /// Rebuild the record from the manager's live connections, after a manual
    /// connect, disconnect or reload. Agents keep their frozen snapshot until
    /// their next intentional recapture.
    pub(crate) async fn sync_mcp_instructions(&self, manager: &RwLock<McpManager>) {
        let manager = manager.read().await;
        let mut by_server = BTreeMap::new();
        for server in manager.connected_servers().await {
            if let Some(text) = manager.server_instructions(&server).await {
                by_server.insert(server, text);
            }
        }
        self.with_record(|record| record.by_server = by_server);
    }

    /// Forget every server's instructions, when an empty config withdraws all
    /// MCP tools without disconnecting the servers.
    pub(crate) fn clear_mcp_instructions(&self) {
        self.with_record(|record| record.by_server.clear());
    }

    /// Wait while initial handshakes are pending, up to the grace deadline.
    pub(crate) async fn wait_for_mcp_instructions(&self) {
        loop {
            let waiting = self.with_record(|record| {
                record.pending > 0 && record.deadline.is_some_and(|d| Instant::now() < d)
            });
            if !waiting {
                return;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Record (or clear, with `None`) one server's initialize instructions.
    pub fn set_mcp_server_instructions(&self, server: &str, instructions: Option<String>) {
        self.with_record(|record| match instructions {
            Some(text) => record.by_server.insert(server.to_string(), text),
            None => record.by_server.remove(server),
        });
    }

    /// `(server, instructions)` for every connected server that sent some,
    /// ordered by server name.
    pub fn mcp_server_instructions(&self) -> Vec<(String, String)> {
        self.with_record(|record| {
            record
                .by_server
                .iter()
                .map(|(server, text)| (server.clone(), text.clone()))
                .collect()
        })
    }
}

#[cfg(test)]
#[path = "mcp_instructions_tests.rs"]
mod tests;
