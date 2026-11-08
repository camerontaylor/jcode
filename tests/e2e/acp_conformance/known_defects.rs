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
    for _ in 0..2 {
        provider.queue_response(vec![
            StreamEvent::TextDelta("answer".into()),
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
        for _ in 0..2 {
            raw.send_message("first-user-marker").await?;
            loop {
                if matches!(raw.read_event().await?, ServerEvent::Done { .. }) {
                    break;
                }
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
            2,
            "two intentionally repeated stored user insertions, two replay items: {users:?}"
        );
        assert_eq!(
            users[0]["params"]["update"]["content"]["text"],
            "first-user-marker"
        );
        let ids: Vec<_> = acp
            .pending
            .iter()
            .filter_map(|m| m["params"]["update"]["messageId"].as_str())
            .map(str::to_owned)
            .collect();
        assert_eq!(ids.len(), 4);
        assert_eq!(
            ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
            4
        );
        assert_eq!(texts(&acp.pending), vec!["answer", "answer"]);
        let kinds: Vec<_> = acp
            .pending
            .iter()
            .filter_map(|m| m["params"]["update"]["sessionUpdate"].as_str())
            .filter(|k| k.ends_with("message_chunk"))
            .collect();
        assert_eq!(
            kinds,
            vec![
                "user_message_chunk",
                "agent_message_chunk",
                "user_message_chunk",
                "agent_message_chunk"
            ]
        );
        acp.take_notifications();
        acp.request(
            "session/load",
            json!({"sessionId":session,"cwd":std::env::current_dir()?}),
        )
        .await?;
        let again: Vec<_> = acp
            .pending
            .iter()
            .filter_map(|m| m["params"]["update"]["messageId"].as_str())
            .collect();
        assert_eq!(again, ids);
        acp_fixture::check("11-import-repeated-user-replay-v1.jsonl", &acp.tape)?;
        acp.shutdown().await;
        Ok(())
    }
    .await;
    task.abort();
    result
}

#[tokio::test]
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
        let mut steering_peer = tokio::net::UnixStream::connect(&socket).await?;
        steering_peer.write_all(format!("{}\n{}\n", json!({"type":"subscribe","id":800,"working_dir":std::env::current_dir()?,"target_session_id":session}), json!({"type":"soft_interrupt","id":801,"content":"reverse-steer-marker","urgent":false,"client_id":"raw-steer"})).as_bytes()).await?;
        // Steering is inserted at the safe point after provider completion,
        // not at admission. Waiting for the echo before release deadlocks the
        // test, and was the cause of this probe's original false failure.
        wait_for_steer_admission(runtime.path()).await?;
        gate.add_permits(2);
        let update = acp
            .wait_for_notification(|m| {
                m["params"]["update"]["sessionUpdate"] == "user_message_chunk"
            })
            .await?;
        assert_eq!(
            update["params"]["update"]["content"]["text"],
            "reverse-steer-marker"
        );
        assert_eq!(
            acp.wait_for_response(prompt).await?["result"]["stopReason"],
            "end_turn"
        );
        assert!(update["params"]["update"]["messageId"].as_str().is_some_and(|id| id.ends_with(":steer:0")), "inserted steer has its own message identity: {update}");
        let updates: Vec<_> = acp
            .tape
            .iter()
            .filter(|r| r["dir"] == "a2c")
            .map(|r| r["msg"].clone())
            .collect();
        assert_eq!(texts(&updates), vec!["before-steer", "after-steer"]);
        let user = updates
            .iter()
            .position(|m| m["params"]["update"]["content"]["text"] == "reverse-steer-marker")
            .unwrap();
        let after = updates
            .iter()
            .position(|m| m["params"]["update"]["content"]["text"] == "after-steer")
            .unwrap();
        assert!(user < after, "steer insertion precedes affected output");
        acp_fixture::check("10-reverse-steer-v1.jsonl", &acp.tape)?;
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
