//! Recover native Codex tool records before provider-safe import normalization.
use super::*;

pub(super) fn may_contain_tool(line: &str) -> bool {
    ["function_call", "custom_tool_call"]
        .iter()
        .any(|kind| line.contains(kind))
}

pub(super) fn append_tool(session: &mut Session, value: &serde_json::Value) -> bool {
    if value.get("type").and_then(|v| v.as_str()) != Some("response_item") {
        return false;
    }
    let Some(payload) = value.get("payload") else {
        return false;
    };
    let kind = payload
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let Some(id) = payload
        .get("call_id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.is_empty())
    else {
        return false;
    };
    let (role, block) = match kind {
        "function_call" | "custom_tool_call" => {
            let Some(name) = payload.get("name").and_then(|v| v.as_str()) else {
                return false;
            };
            let input = if kind == "function_call" {
                match payload.get("arguments") {
                    Some(serde_json::Value::String(raw)) => serde_json::from_str(raw)
                        .unwrap_or_else(|_| serde_json::json!({"arguments": raw})),
                    Some(input) => input.clone(),
                    None => serde_json::json!({}),
                }
            } else {
                serde_json::json!({"input": payload.get("input").cloned().unwrap_or_default()})
            };
            (
                Role::Assistant,
                ContentBlock::ToolUse {
                    id: id.into(),
                    name: name.into(),
                    input,
                    thought_signature: None,
                },
            )
        }
        "function_call_output" | "custom_tool_call_output" => {
            let output = payload.get("output").cloned().unwrap_or_default();
            (
                Role::User,
                ContentBlock::ToolResult {
                    tool_use_id: id.into(),
                    content: extract_text_from_json_value(&output),
                    is_error: None,
                },
            )
        }
        _ => return false,
    };
    session.append_stored_message(StoredMessage {
        id: crate::id::new_id("msg"),
        role,
        content: vec![block],
        display_role: None,
        timestamp: parse_rfc3339_json(value.get("timestamp")),
        tool_duration_ms: None,
        token_usage: None,
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_import_preserves_native_tool_calls_and_outputs() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::TempDir::new().unwrap();
        struct RestoreHome(Option<std::ffi::OsString>);
        impl Drop for RestoreHome {
            fn drop(&mut self) {
                if let Some(home) = self.0.take() {
                    crate::env::set_var("JCODE_HOME", home);
                } else {
                    crate::env::remove_var("JCODE_HOME");
                }
            }
        }
        let _home = RestoreHome(std::env::var_os("JCODE_HOME"));
        crate::env::set_var("JCODE_HOME", temp.path());
        let path = temp.path().join("rollout.jsonl");
        let records = [
            serde_json::json!({"type":"session_meta","payload":{"id":"tool-history","cwd":"/repo"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Attach this session"}]}}),
            serde_json::json!({"type":"response_item","payload":{"type":"function_call","call_id":"f1","name":"bash","arguments":"{\"command\":\"pwd\"}"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"f1","output":"/repo"}}),
            serde_json::json!({"type":"response_item","timestamp":"2026-11-11T06:04:50Z","payload":{"type":"custom_tool_call","call_id":"c1","name":"exec","input":"text(await tools.exec_command({cmd:'helper'}));"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"c1","output":[{"type":"input_text","text":"JCODE_SESSION_ID is unset"}]}}),
        ];
        std::fs::write(
            &path,
            records
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let session = import_codex_session_from_path(&path, None).unwrap();
        assert_eq!(session.messages.len(), 5);
        assert_eq!(session.working_dir.as_deref(), Some("/repo"));
        assert_eq!(session.title.as_deref(), Some("Attach this session"));
        // Imports deliberately flatten tools, so arbitrary external tool names
        // cannot be replayed as executable calls to a different provider.
        let text = |index: usize| match &session.messages[index].content[0] {
            ContentBlock::Text { text, .. } => text.clone(),
            _ => panic!("import must retain provider-safe text"),
        };
        assert!(text(1).contains("[Imported tool call: bash]"));
        assert!(text(1).contains("\"command\":\"pwd\""));
        assert!(text(2).contains("[Imported tool result: f1]"));
        assert!(text(2).contains("/repo"));
        assert!(text(3).contains("[Imported tool call: exec]"));
        assert!(text(3).contains("helper"));
        assert!(session.messages[3].timestamp.is_some());
        assert!(text(4).contains("[Imported tool result: c1]"));
        assert!(text(4).contains("JCODE_SESSION_ID is unset"));
    }
}
