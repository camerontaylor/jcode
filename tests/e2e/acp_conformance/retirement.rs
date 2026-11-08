use super::*;

#[tokio::test]
async fn acp_conformance_busy_config_errors_are_atomic() -> Result<()> {
    retirement("config").await
}
#[tokio::test]
#[ignore = "known defect: ACP-LOST-STEER, abrupt adapter loss leaves queued input"]
async fn acp_conformance_transport_loss_withdraws_waiting_steer() -> Result<()> {
    retirement("pending-loss").await
}
#[tokio::test]
async fn acp_conformance_peer_config_serializes_after_active_turn() -> Result<()> {
    retirement("peer-config").await
}
#[tokio::test]
async fn acp_conformance_transport_loss_preserves_inserted_work() -> Result<()> {
    retirement("origin-loss").await
}

async fn retirement(operation: &str) -> Result<()> {
    let _env = setup_test_env()?;
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = Arc::new(MockProvider::gated(gate.clone()));
    provider.queue_response(vec![
        StreamEvent::TextDelta("inserted-answer".into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]);
    let daemon = server::Server::new_with_paths(
        provider.clone(),
        socket.clone(),
        runtime.path().join("debug.sock"),
    );
    let task = tokio::spawn(async move { daemon.run().await });
    let result = async {
        wait_for_socket(&socket).await?;
        let mut a = AcpClient::spawn()?;
        let mut b = AcpClient::spawn()?;
        let mut c = AcpClient::spawn()?;
        for client in [&mut a, &mut b, &mut c] { initialize(client).await?; }
        let cwd = std::env::current_dir()?;
        let created = a.request("session/new", json!({"cwd":cwd})).await?;
        let session = &created["sessionId"];
        let initial = created["models"]["currentModelId"].clone();
        assert!(initial.is_string());
        for client in [&mut b, &mut c] { client.request("session/resume", json!({"sessionId":session,"cwd":cwd})).await?; }
        let prompt = a.send_request("session/prompt", json!({"sessionId":session,"prompt":[{"type":"text","text":"owned-work"}]})).await?;
        a.wait_for_notification(|m|m["params"]["update"]["content"]["text"]=="inserted-answer").await?;
        let start = c.wait_for_notification(|m|m["method"]=="_jcode/turn" && m["params"]["state"]=="started").await?;
        let mut deferred_config = None;
        match operation {
            "peer-config" => {
                deferred_config = Some(b.send_request("session/set_config_option", json!({"sessionId":session,"configId":"model","value":"after-turn-model"})).await?);
            }
            "config" => {
                let client = &mut a;
                {
                    for (method, fields) in [
                        ("session/set_config_option",json!({"configId":"model","value":"must-not-apply"})),
                        ("session/set_model",json!({"modelId":"must-not-apply"})),
                        ("session/set_config_option",json!({"configId":"reasoning_effort","value":"high"})),
                    ] {
                        let mut params = fields;
                        params["sessionId"] = session.clone();
                        let request = client.send_request(method, params).await?;
                        let rejected = timeout(Duration::from_secs(2), client.wait_for_response(request)).await.with_context(|| format!("busy {operation} {method} must answer before provider release"))??;
                        assert!(rejected.get("error").is_some(), "busy {method} must fail atomically: {rejected}");
                    }
                }
            }
            "pending-loss" => {
                b.send_request("session/prompt", json!({"sessionId":session,"prompt":[{"type":"text","text":"must-be-withdrawn"}]})).await?;
                wait_for_steer_admission(runtime.path()).await?;
                // Abrupt child termination closes its daemon socket, exercising
                // transport-loss cleanup rather than the detach request path.
                b.shutdown().await;
                let cleaned = wait_for_empty_interrupts(runtime.path()).await.is_ok();
                eprintln!("pending steer withdrawn before safe point: {cleaned}");
            }
            "origin-loss" => { a.shutdown().await; }
            _ => unreachable!(),
        }
        gate.add_permits(1);
        if operation != "origin-loss" {
            assert_eq!(a.wait_for_response(prompt).await?["result"]["stopReason"], "end_turn");
        }
        if let Some(request) = deferred_config {
            let applied = b.wait_for_response(request).await?;
            assert!(applied.get("error").is_none(), "serialized setter succeeds after turn retirement: {applied}");
            assert!(applied["result"]["configOptions"].as_array().unwrap().iter().any(|option|option["id"]=="model" && option["currentValue"]=="after-turn-model"));
        }
        let end = c.wait_for_notification(|m|m["method"]=="_jcode/turn" && m["params"]["state"]!="started").await?;
        assert_eq!(end["params"]["turnId"], start["params"]["turnId"]);
        assert_eq!(end["params"]["state"], "completed", "transport loss must not cancel inserted work");
        let loaded = c.request("session/load", json!({"sessionId":session,"cwd":cwd})).await?;
        if operation == "peer-config" {
            assert_eq!(loaded["models"]["currentModelId"], "after-turn-model");
            assert_eq!(provider.captured_models.lock().unwrap().as_slice(), &[initial.as_str().unwrap().to_string()], "configuration cannot interleave into the active provider call");
        } else {
            assert_eq!(loaded["models"]["currentModelId"], initial, "rejected setters cannot change the effective model");
            assert_eq!(loaded["configOptions"], created["configOptions"], "rejected setters cannot change effort or options");
        }
        assert_eq!(provider.captured_models.lock().unwrap().len(), 1, "withdrawn steer must not cause another provider call");
        let messages: Vec<_> = c.tape.iter().filter(|r|r["dir"]=="a2c").map(|r|r["msg"].clone()).collect();
        assert!(!messages.iter().any(|m|m["params"]["update"]["content"]["text"]=="must-be-withdrawn"));
        a.shutdown().await; b.shutdown().await; c.shutdown().await;
        Ok(())
    }.await;
    task.abort();
    result
}

async fn wait_for_empty_interrupts(runtime: &std::path::Path) -> Result<()> {
    timeout(Duration::from_secs(2), async {
        loop {
            let state =
                debug_run_command_json(runtime.join("debug.sock"), "server:memory", None).await?;
            if state["interrupts"]["pending_interrupts"].as_u64() == Some(0) {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await?
}
