use super::*;

// Exercise a legacy daemon through the existing socket-stub seam, without
// adding capability overrides to the shipped binary.
#[cfg(unix)]
#[tokio::test]
async fn acp_conformance_negotiation_legacy_v1() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let _env = setup_test_env()?;
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
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
                        "subscribe" => vec![json!({"type":"done","id":id})],
                        "get_history" => vec![json!({"type":"history","id":id,"session_id":"legacy-test","messages":[],"all_sessions":["legacy-test"]})],
                        "message" => vec![json!({"type":"text_delta","id":id,"text":"legacy answer"}), json!({"type":"done","id":id})],
                        _ => panic!("unexpected legacy request: {request}"),
                    };
                    for response in responses { write.write_all(format!("{response}\n").as_bytes()).await.unwrap(); }
                }
            });
        }
    });
    let result = async {
        let mut c = AcpClient::spawn()?;
        let r = c.request("initialize", json!({"protocolVersion":999,"clientCapabilities":{"futureCapability":{"enabled":true}},"futureField":true})).await?;
        assert_eq!(r["protocolVersion"], 1);
        let meta = &r["agentCapabilities"]["_meta"]["jcode"];
        assert_eq!(meta["interfaceVersion"], 1);
        assert_eq!(meta["sessionMcpServers"], false);
        assert_eq!(meta["profile"], "standard");
        let new = c.request("session/new", json!({"cwd":std::env::current_dir()?})).await?;
        let session = &new["sessionId"];
        for key in ["observe", "stopObservedTurn", "implicitSteering", "detach"] {
            assert_eq!(new["_meta"]["jcode"]["sharedControl"][key], false, "{new}");
        }
        for (method, params, code) in [
            ("_jcode/future", json!({}), -32601),
            ("_jcode/detach", json!({"sessionId":42}), -32602),
            ("_jcode/detach", json!({"sessionId":session}), -32601),
        ] {
            let id = c.send_request(method, params).await?;
            let response = c.wait_for_response(id).await?;
            assert_eq!(response["error"]["code"], code, "{response}");
        }
        let baseline = c.request("session/prompt", json!({"sessionId":session,"prompt":[{"type":"text","text":"legacy prompt"}]})).await?;
        let response = c.request("session/prompt", json!({"sessionId":session,"prompt":[{"type":"text","text":"legacy prompt"}],"_meta":{"future":{"enabled":true}}})).await?;
        assert_eq!(response, baseline);
        assert_eq!(response["stopReason"], "end_turn");
        assert!(response.pointer("/_meta/jcode/turnId").is_none());
        assert!(!c.pending.iter().any(|m| m["method"] == "_jcode/turn"));
        acp_fixture::check("08-negotiation-legacy-v1.jsonl", &c.tape)?;
        c.shutdown().await;
        Ok(())
    }.await;
    daemon.abort();
    result
}
