use super::*;

/// Enumerate owner, terminal action and observer attachment timing. Each case
/// holds output at a provider barrier, with no timing assumptions. Failures
/// identify the complete (three-operation) sequence, already small enough to
/// replay directly without a random shrinker.
#[tokio::test]
async fn acp_conformance_interleavings() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = Arc::new(MockProvider::gated(gate.clone()));
    let daemon = server::Server::new_with_paths(
        provider.clone(),
        socket.clone(),
        runtime.path().join("debug.sock"),
    );
    let task = tokio::spawn(async move { daemon.run().await });
    let result = async {
        wait_for_socket(&socket).await?;
        for owner in 0..2 {
            for action in ["complete", "cancel", "close"] {
                for midturn in [false, true] {
                    let sequence = format!("owner={owner}, prompt, attach(midturn={midturn}), {action}");
                    let case = async {
                        let mut a = AcpClient::spawn()?;
                        let mut b = AcpClient::spawn()?;
                        initialize(&mut a).await?;
                        initialize(&mut b).await?;
                        let (origin, observer) = if owner == 0 { (&mut a, &mut b) } else { (&mut b, &mut a) };
                        let cwd = std::env::current_dir()?;
                        let created = origin.request("session/new", json!({"cwd":cwd})).await?;
                        let id = &created["sessionId"];
                        if !midturn {
                            observer.request("session/resume", json!({"sessionId":id,"cwd":cwd})).await?;
                        }
                        let text = format!("answer-{owner}-{action}-{midturn}");
                        provider.queue_response(vec![
                            StreamEvent::TextDelta(text.clone()),
                            StreamEvent::MessageEnd { stop_reason: Some("end_turn".into()) },
                        ]);
                        let prompt = origin.send_request("session/prompt", json!({"sessionId":id,"prompt":[{"type":"text","text":"question"}]})).await?;
                        origin.wait_for_notification(|m| m["params"]["update"]["content"]["text"] == text).await?;
                        if midturn {
                            observer.request("session/resume", json!({"sessionId":id,"cwd":cwd})).await?;
                        }
                        let start = observer.wait_for_notification(|m| m["method"] == "_jcode/turn" && m["params"]["state"] == "started").await?;
                        assert!(start["params"]["origin"].is_object());
                        match action {
                            "complete" => gate.add_permits(1),
                            "cancel" => origin.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}})).await?,
                            _ => { origin.request("session/close", json!({"sessionId":id})).await?; }
                        }
                        let response = origin.wait_for_response(prompt).await?;
                        assert_eq!(response["result"]["_meta"]["jcode"], json!({"turnId":start["params"]["turnId"],"role":"origin"}));
                        let expected = if action == "complete" { "completed" } else { "canceled" };
                        let end = observer.wait_for_notification(|m| m["method"] == "_jcode/turn" && m["params"]["state"] != "started").await?;
                        assert_eq!(end["params"]["state"], expected);
                        assert_eq!(end["params"]["turnId"], start["params"]["turnId"]);
                        // An answered initialize is an adapter processing barrier.
                        initialize(observer).await?;
                        let lifecycle: Vec<_> = observer.tape.iter().filter(|r| r["msg"]["method"] == "_jcode/turn").collect();
                        assert_eq!(lifecycle.len(), 2, "exactly one non-nested pair");
                        let received: Vec<_> = origin.tape.iter().filter(|r| r["dir"] == "a2c").map(|r| r["msg"].clone()).collect();
                        assert_eq!(texts(&received), vec![text]);
                        assert!(origin.pending.iter().all(|m| m.get("method").is_some()), "every request answered");
                        a.shutdown().await;
                        b.shutdown().await;
                        Ok::<(), anyhow::Error>(())
                    }.await;
                    case.with_context(|| sequence)?;
                }
            }
        }
        Ok(())
    }.await;
    task.abort();
    result
}
