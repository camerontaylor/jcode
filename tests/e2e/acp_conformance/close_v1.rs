use super::*;

#[tokio::test]
async fn acp_conformance_close_owner_aware_v1() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = Arc::new(GatedProvider {
        gate: gate.clone(),
        calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let server =
        server::Server::new_with_paths(provider, socket.clone(), runtime.path().join("debug.sock"));
    let task = tokio::spawn(async move { server.run().await });
    let result = async {
        wait_for_socket(&socket).await?;
        let mut a = AcpClient::spawn()?;
        let mut b = AcpClient::spawn()?;
        let mut c = AcpClient::spawn()?;
        initialize(&mut a).await?;
        initialize(&mut b).await?;
        initialize(&mut c).await?;
        let cwd = std::env::current_dir()?;
        let new = a.request("session/new", json!({"cwd":cwd})).await?;
        let id = &new["sessionId"];
        b.request("session/resume", json!({"sessionId":id,"cwd":cwd}))
            .await?;
        c.request("session/resume", json!({"sessionId":id,"cwd":cwd}))
            .await?;
        let first = a
            .send_request(
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"owner close"}]}),
            )
            .await?;
        let start = b
            .wait_for_notification(|m| {
                m["method"] == "_jcode/turn" && m["params"]["state"] == "started"
            })
            .await?;
        b.wait_for_notification(|m| m["params"]["update"]["content"]["text"] == "answer-0")
            .await?;
        a.request("session/close", json!({"sessionId":id})).await?;
        let response = a.wait_for_response(first).await?;
        assert_eq!(response["result"]["stopReason"], "cancelled");
        assert_eq!(
            response["result"]["_meta"]["jcode"],
            json!({"turnId":start["params"]["turnId"],"role":"origin"})
        );
        let end = b
            .wait_for_notification(|m| {
                m["method"] == "_jcode/turn" && m["params"]["state"] != "started"
            })
            .await?;
        assert_eq!(end["params"]["state"], "canceled");
        assert_eq!(end["params"]["turnId"], start["params"]["turnId"]);
        a.request("session/close", json!({"sessionId":id})).await?;
        a.request("session/resume", json!({"sessionId":id,"cwd":cwd}))
            .await?;
        let second = a
            .send_request(
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"observer close"}]}),
            )
            .await?;
        let start = b
            .wait_for_notification(|m| {
                m["method"] == "_jcode/turn" && m["params"]["state"] == "started"
            })
            .await?;
        b.wait_for_notification(|m| m["params"]["update"]["content"]["text"] == "answer-1")
            .await?;
        b.request("session/close", json!({"sessionId":id})).await?;
        let ends: Vec<_> = b
            .pending
            .iter()
            .filter(|m| m["method"] == "_jcode/turn")
            .collect();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0]["params"]["state"], "canceled");
        assert_eq!(ends[0]["params"]["turnId"], start["params"]["turnId"]);
        b.request("session/close", json!({"sessionId":id})).await?;
        gate.add_permits(1);
        assert_eq!(
            a.wait_for_response(second).await?["result"]["stopReason"],
            "end_turn"
        );
        let end = c
            .wait_for_notification(|m| {
                m["method"] == "_jcode/turn"
                    && m["params"]["turnId"] == start["params"]["turnId"]
                    && m["params"]["state"] != "started"
            })
            .await?;
        assert_eq!(end["params"]["state"], "completed");
        initialize(&mut b).await?;
        let terminals = b
            .tape
            .iter()
            .filter(|r| {
                r["dir"] == "a2c"
                    && r["msg"]["method"] == "_jcode/turn"
                    && r["msg"]["params"]["state"] != "started"
            })
            .count();
        assert_eq!(terminals, 2);
        acp_fixture::check("05-close-owner-aware-v1.jsonl", &b.tape)?;
        a.shutdown().await;
        b.shutdown().await;
        c.shutdown().await;
        Ok(())
    }
    .await;
    task.abort();
    result
}
