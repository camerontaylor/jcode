//! Replay through the real stdio adapter, not just its JSON mapper.
use super::*;

#[cfg(unix)]
#[tokio::test]
async fn acp_load_replays_typed_history_blocks() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let _env = setup_test_env()?;
    let runtime = tempfile::Builder::new().prefix("acp-replay-").tempdir()?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let _socket = EnvVarGuard::set("JCODE_SOCKET", runtime.path().join("jcode.sock"));
    let listener = tokio::net::UnixListener::bind(runtime.path().join("jcode.sock"))?;
    let daemon = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            connections.spawn(async move {
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read).lines();
                while let Some(line) = lines.next_line().await.unwrap() {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let id = &request["id"];
                    let responses = match request["type"].as_str().unwrap() {
                        "ping" => vec![json!({"type":"pong","id":id,"capabilities":[]})],
                        "subscribe" | "get_history" => vec![
                            json!({"type":"history","id":id,"session_id":"replay-test","messages":[
                                {"role":"user","content":"Read this file","stored_index":0},
                                {"role":"tool","content":"raw read result","stored_index":2,
                                 "tool_data":{"id":"read-1","name":"read","input":{"file_path":"/workspace/file"}}},
                                {"role":"assistant","content":format!("{}\nAnswer", jcode_render_core::reasoning_line_markup("Reason **carefully**")),"stored_index":3},
                                {"role":"error","content":"provider diagnostic"}
                            ]}),
                            json!({"type":"done","id":id}),
                        ],
                        "prepare_disconnect" => vec![json!({"type":"done","id":id})],
                        _ => panic!("unexpected request: {request}"),
                    };
                    for response in responses {
                        write.write_all(format!("{response}\n").as_bytes()).await.unwrap();
                    }
                }
            });
        }
    });
    let result = async {
        let mut client = AcpClient::spawn()?;
        initialize(&mut client).await?;
        client
            .request(
                "session/load",
                json!({"sessionId":"replay-test","cwd":std::env::current_dir()?}),
            )
            .await?;
        let notifications = client.take_notifications();
        let updates: Vec<&Value> = notifications
            .iter()
            .filter(|event| event["method"] == "session/update")
            .map(|event| &event["params"]["update"])
            .collect();
        let tool = updates
            .iter()
            .find(|update| update["sessionUpdate"] == "tool_call")
            .with_context(|| format!("replayed tool missing from {notifications:#?}"))?;
        assert_eq!(tool["toolCallId"], "read-1");
        assert_eq!(tool["status"], "completed");
        assert_eq!(tool["content"][0]["content"]["text"], "raw read result");
        let thought = updates
            .iter()
            .find(|update| update["sessionUpdate"] == "agent_thought_chunk")
            .context("replayed thought")?;
        assert_eq!(thought["content"]["text"], "Reason **carefully**\n");
        let answers: Vec<&str> = updates
            .iter()
            .filter(|update| update["sessionUpdate"] == "agent_message_chunk")
            .filter_map(|update| update["content"]["text"].as_str())
            .collect();
        assert_eq!(answers, vec!["\nAnswer"]);
        assert!(
            !updates
                .iter()
                .any(|update| update["sessionUpdate"] == "notice")
        );
        client.shutdown().await;
        let mut client = AcpClient::spawn()?;
        client
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {"session": {"notices": {}}}
                }),
            )
            .await?;
        client
            .request(
                "session/load",
                json!({
                    "sessionId": "replay-test", "cwd": std::env::current_dir()?
                }),
            )
            .await?;
        let notifications = client.take_notifications();
        let notice = notifications
            .iter()
            .find(|event| {
                event["method"] == "session/update"
                    && event["params"]["update"]["sessionUpdate"] == "notice"
            })
            .context("negotiated error notice")?;
        assert_eq!(notice["params"]["update"]["severity"], "error");
        assert_eq!(
            notice["params"]["update"]["description"],
            "provider diagnostic"
        );
        client.shutdown().await;
        Ok(())
    }
    .await;
    daemon.abort();
    result
}
