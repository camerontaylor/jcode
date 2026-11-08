use super::*;

/// Narrow regression probe for the independently observed load duplicate.
/// This simple single-message history passes; the imported live QA history
/// needs a richer reproducer in the defect lane.
#[tokio::test]
async fn acp_known_defect_replay_first_user_once() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let provider = Arc::new(MockProvider::new());
    provider.queue_response(vec![
        StreamEvent::TextDelta("answer".into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]);
    let daemon =
        server::Server::new_with_paths(provider, socket.clone(), runtime.path().join("debug.sock"));
    let task = tokio::spawn(async move { daemon.run().await });
    let result = async {
        wait_for_socket(&socket).await?;
        let mut raw = wait_for_subscribed_server_client(&socket).await?;
        let session = match raw.get_history_event().await? {
            ServerEvent::History { session_id, .. } => session_id,
            event => anyhow::bail!("expected history: {event:?}"),
        };
        raw.send_message("first-user-marker").await?;
        loop {
            if matches!(raw.read_event().await?, ServerEvent::Done { .. }) {
                break;
            }
        }
        let mut acp = AcpClient::spawn()?;
        initialize(&mut acp).await?;
        acp.request(
            "session/load",
            json!({"sessionId":session,"cwd":std::env::current_dir()?}),
        )
        .await?;
        let users: Vec<_> = acp
            .pending
            .iter()
            .filter(|m| m["params"]["update"]["sessionUpdate"] == "user_message_chunk")
            .collect();
        assert_eq!(
            users.len(),
            1,
            "one stored user insertion, one replay item: {users:?}"
        );
        assert_eq!(
            users[0]["params"]["update"]["content"]["text"],
            "first-user-marker"
        );
        acp.shutdown().await;
        Ok(())
    }
    .await;
    task.abort();
    result
}

#[tokio::test]
#[ignore = "known defect: QA-STEER, see fork/acp-e2e-defects"]
async fn acp_known_defect_reverse_steer_is_mirrored() -> Result<()> {
    reverse_steer().await
}

async fn reverse_steer() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = Arc::new(MockProvider::gated(gate.clone()));
    for text in ["before-steer", "after-steer"] {
        provider.queue_response(vec![
            StreamEvent::TextDelta(text.into()),
            StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            },
        ]);
    }
    let daemon =
        server::Server::new_with_paths(provider, socket.clone(), runtime.path().join("debug.sock"));
    let task = tokio::spawn(async move { daemon.run().await });
    let result = async {
        wait_for_socket(&socket).await?;
        let mut raw = wait_for_subscribed_server_client(&socket).await?;
        let session = match raw.get_history_event().await? {
            ServerEvent::History { session_id, .. } => session_id,
            event => anyhow::bail!("expected history: {event:?}"),
        };
        let mut acp = AcpClient::spawn()?;
        initialize(&mut acp).await?;
        acp.request(
            "session/resume",
            json!({"sessionId":session,"cwd":std::env::current_dir()?}),
        )
        .await?;
        let prompt = acp
            .send_request(
                "session/prompt",
                json!({"sessionId":session,"prompt":[{"type":"text","text":"own-question"}]}),
            )
            .await?;
        acp.wait_for_notification(|m| m["params"]["update"]["content"]["text"] == "before-steer")
            .await?;
        raw.send_message("reverse-steer-marker").await?;
        // Require the insertion echo while the provider remains gated; this
        // prevents a late new turn from passing as a same-turn steer.
        let update = acp
            .wait_for_notification(|m| {
                m["params"]["update"]["sessionUpdate"] == "user_message_chunk"
            })
            .await?;
        assert_eq!(
            update["params"]["update"]["content"]["text"],
            "reverse-steer-marker"
        );
        gate.add_permits(2);
        assert_eq!(
            acp.wait_for_response(prompt).await?["result"]["stopReason"],
            "end_turn"
        );
        acp.shutdown().await;
        Ok(())
    }
    .await;
    task.abort();
    result
}

/// Provider-stream version of the observed Stop assertion. The deployed QA
/// failure was during a tool wait; passing this narrower probe does not close it.
#[tokio::test]
async fn acp_known_defect_stop_observed_peer() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = Arc::new(MockProvider::gated(gate));
    provider.queue_response(vec![
        StreamEvent::TextDelta("running".into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]);
    let daemon =
        server::Server::new_with_paths(provider, socket.clone(), runtime.path().join("debug.sock"));
    let task = tokio::spawn(async move { daemon.run().await });
    let result = async {
        wait_for_socket(&socket).await?;
        let mut raw = wait_for_subscribed_server_client(&socket).await?;
        let session = match raw.get_history_event().await? {
            ServerEvent::History { session_id, .. } => session_id,
            event => anyhow::bail!("expected history: {event:?}"),
        };
        let mut acp = AcpClient::spawn()?;
        initialize(&mut acp).await?;
        let attached = acp
            .request(
                "session/resume",
                json!({"sessionId":session,"cwd":std::env::current_dir()?}),
            )
            .await?;
        assert_eq!(
            attached["_meta"]["jcode"]["sharedControl"]["stopObservedTurn"],
            true
        );
        raw.send_message("peer-question").await?;
        let start = acp
            .wait_for_notification(|m| {
                m["method"] == "_jcode/turn" && m["params"]["state"] == "started"
            })
            .await?;
        acp.wait_for_notification(|m| m["params"]["update"]["content"]["text"] == "running")
            .await?;
        acp.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}))
            .await?;
        let end = acp
            .wait_for_notification(|m| {
                m["method"] == "_jcode/turn" && m["params"]["state"] != "started"
            })
            .await?;
        assert_eq!(end["params"]["turnId"], start["params"]["turnId"]);
        assert_eq!(end["params"]["state"], "canceled");
        let mut interrupted = false;
        loop {
            match timeout(Duration::from_secs(5), raw.read_event()).await?? {
                ServerEvent::Done { .. } => break,
                event => {
                    interrupted |= format!("{event:?}").contains("Interrupted");
                }
            }
        }
        assert!(interrupted, "originator must observe interrupted execution");
        acp.shutdown().await;
        Ok(())
    }
    .await;
    task.abort();
    result
}
