use crate::mcp::{McpConfig, McpManager};
use crate::tool::mcp::McpManagementTool;
use crate::tool::{Registry, Tool, ToolContext, ToolExecutionMode};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Minimal stdio MCP server: answers initialize with argv[1] as instructions.
const SERVER: &str = r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    method = req.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "t", "version": "1"}, "instructions": sys.argv[1]}
    elif method == "tools/list":
        result = {"tools": []}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
"#;

fn context() -> ToolContext {
    ToolContext {
        session_id: "test-session".into(),
        message_id: "test-message".into(),
        tool_call_id: "test-tool-call".into(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Manual connect and disconnect keep the registry record in step with the
/// live connections, so a later recapture never shows stale guidance.
#[tokio::test]
async fn manual_connect_and_disconnect_sync_the_record() {
    if !python3_available() {
        eprintln!("SKIP: python3 unavailable for real MCP stdio test");
        return;
    }
    let _guard = crate::storage::lock_test_env();
    let registry = Registry::empty();
    let manager = Arc::new(RwLock::new(McpManager::with_config(McpConfig::default())));
    let tool = McpManagementTool::new(manager).with_registry(registry.clone());
    registry.set_mcp_server_instructions("stale", Some("Old guidance.".into()));

    let output = tool
        .execute(
            json!({"action": "connect", "server": "live", "command": "python3",
                   "args": ["-I", "-S", "-u", "-c", SERVER, "Live guidance."]}),
            context(),
        )
        .await
        .expect("connect");
    assert!(
        output.output.contains("Connected to MCP server"),
        "{}",
        output.output
    );
    assert_eq!(
        registry.mcp_server_instructions(),
        vec![("live".to_string(), "Live guidance.".to_string())]
    );

    tool.execute(json!({"action": "disconnect", "server": "live"}), context())
        .await
        .expect("disconnect");
    assert!(registry.mcp_server_instructions().is_empty());
}

/// Reloading an empty config withdraws every MCP tool but leaves connections
/// up; the record must still forget their instructions.
#[tokio::test]
async fn empty_config_reload_clears_the_record() {
    if !python3_available() {
        eprintln!("SKIP: python3 unavailable for real MCP stdio test");
        return;
    }
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let previous: Vec<_> = ["JCODE_HOME", "JCODE_DISABLE_CLAUDE_MCP"]
        .iter()
        .map(|key| (*key, std::env::var_os(key)))
        .collect();
    crate::env::set_var("JCODE_HOME", home.path());
    crate::env::set_var("JCODE_DISABLE_CLAUDE_MCP", "1");

    let registry = Registry::empty();
    let manager = Arc::new(RwLock::new(McpManager::with_config(McpConfig::default())));
    let tool = McpManagementTool::new(manager).with_registry(registry.clone());
    tool.execute(
        json!({"action": "connect", "server": "live", "command": "python3",
               "args": ["-I", "-S", "-u", "-c", SERVER, "Live guidance."]}),
        context(),
    )
    .await
    .expect("connect");
    assert_eq!(registry.mcp_server_instructions().len(), 1);

    let output = tool
        .execute(json!({"action": "reload"}), context())
        .await
        .expect("reload");
    let cleared = registry.mcp_server_instructions().is_empty();
    tool.execute(json!({"action": "disconnect", "server": "live"}), context())
        .await
        .expect("disconnect");
    for (key, value) in previous {
        match value {
            Some(value) => crate::env::set_var(key, value),
            None => crate::env::remove_var(key),
        }
    }
    assert!(
        output.output.contains("No servers found"),
        "{}",
        output.output
    );
    assert!(cleared);
}
