use super::*;

/// Small replayable schedules: the seed chooses the originator, attachment
/// timing, and observer departure; all six terminal operations run per seed.
/// Each failure reports the seed and one independent minimal schedule, rather
/// than a long history whose earlier operations can influence reproduction.
#[tokio::test]
async fn acp_conformance_seeded_three_client_invariants() -> Result<()> {
    schedules(false).await
}

#[tokio::test]
#[ignore = "known defect: ACP-LOAD-BRACKET, repeated mid-turn load starts a nested bracket"]
async fn acp_conformance_repeated_midturn_load_has_one_bracket() -> Result<()> {
    schedules(true).await
}

async fn schedules(repeated_load: bool) -> Result<()> {
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
        let count: u64 = std::env::var("JCODE_ACP_INTERLEAVING_SEEDS").unwrap_or_else(|_| "4".into()).parse()?;
        for seed in 0..count {
            let mut random = seed.wrapping_add(1);
            let operations: &[&str] = if repeated_load { &["load"] } else { &["complete", "steer", "cancel", "detach", "close", "load"] };
            for &operation in operations {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                let owner = (random >> 32) & 1;
                let late = random & 1 == 0;
                let observer_leaves = random & 2 == 0;
                let context = format!("seed={seed}: owner={owner}; prompt; attach(late={late}); observer_leave={observer_leaves}; {operation}; stale_cancel; successor");
                eprintln!("{context}");
                let case = async {
                    let mut a = AcpClient::spawn()?;
                    let mut b = AcpClient::spawn()?;
                    let mut witness = AcpClient::spawn()?;
                    for client in [&mut a, &mut b, &mut witness] { initialize(client).await?; }
                    let (origin, observer) = if owner == 0 { (&mut a, &mut b) } else { (&mut b, &mut a) };
                    let cwd = std::env::current_dir()?;
                    let created = origin.request("session/new", json!({"cwd":cwd})).await?;
                    let session = &created["sessionId"];
                    witness.request("session/resume", json!({"sessionId":session,"cwd":cwd})).await?;
                    if !late { observer.request("session/load", json!({"sessionId":session,"cwd":cwd})).await?; }
                    let before = format!("seed-{seed}-{operation}-before");
                    let after = format!("seed-{seed}-{operation}-after");
                    queue_turn(&provider, &before);
                    let request = origin.send_request("session/prompt", json!({"sessionId":session,"prompt":[{"type":"text","text":"origin"}]})).await?;
                    origin.wait_for_notification(|m| is_text(m, &before)).await?;
                    let started = witness.wait_for_notification(is_start).await?;
                    let turn = &started["params"]["turnId"];
                    if late { observer.request("session/load", json!({"sessionId":session,"cwd":cwd})).await?; }
                    if observer_leaves {
                        observer.request("_jcode/detach", json!({"sessionId":session})).await?;
                        observer.request("session/resume", json!({"sessionId":session,"cwd":cwd})).await?;
                    }
                    let mut steer = None;
                    match operation {
                        "steer" => {
                            queue_turn(&provider, &after);
                            steer = Some(observer.send_request("session/prompt", json!({"sessionId":session,"prompt":[{"type":"text","text":"steer-marker"}]})).await?);
                            wait_for_steer_admission(runtime.path()).await?;
                            gate.add_permits(2);
                        }
                        "cancel" => { origin.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}})).await?; }
                        "close" => { origin.request("session/close", json!({"sessionId":session})).await?; }
                        "detach" => { origin.request("_jcode/detach", json!({"sessionId":session})).await?; gate.add_permits(1); }
                        "load" => { if !repeated_load { observer.request("_jcode/detach", json!({"sessionId":session})).await?; } observer.request("session/load", json!({"sessionId":session,"cwd":cwd})).await?; gate.add_permits(1); }
                        _ => gate.add_permits(1),
                    }
                    let response = origin.wait_for_response(request).await?;
                    assert_eq!(response["result"]["_meta"]["jcode"], json!({"turnId":turn,"role":"origin"}), "{context}");
                    let local_stop = matches!(operation, "cancel" | "close" | "detach");
                    assert_eq!(response["result"]["stopReason"], if local_stop {"cancelled"} else {"end_turn"}, "{context}");
                    if let Some(request) = steer {
                        let joined = observer.wait_for_response(request).await?;
                        assert_eq!(joined["result"]["_meta"]["jcode"], json!({"turnId":turn,"role":"steer"}), "{context}");
                        assert_eq!(joined["result"]["stopReason"], "end_turn", "{context}");
                    }
                    let ended = witness.wait_for_notification(is_end).await?;
                    assert_eq!(ended["params"]["turnId"], *turn, "{context}");
                    assert_eq!(ended["params"]["state"], if matches!(operation,"cancel"|"close") {"canceled"} else {"completed"}, "{context}");
                    // Reattach after local retirement and fence a stale idle Stop
                    // with initialize before starting a distinct successor.
                    if matches!(operation, "close" | "detach") { origin.request("session/resume", json!({"sessionId":session,"cwd":cwd})).await?; }
                    origin.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}})).await?;
                    initialize(origin).await?;
                    queue_turn(&provider, "successor-only");
                    let successor = origin.send_request("session/prompt", json!({"sessionId":session,"prompt":[{"type":"text","text":"successor"}]})).await?;
                    origin.wait_for_notification(|m| is_text(m, "successor-only")).await?;
                    let next_start = witness.wait_for_notification(is_start).await?;
                    assert_ne!(next_start["params"]["turnId"], *turn, "{context}");
                    gate.add_permits(1);
                    let next = origin.wait_for_response(successor).await?;
                    assert_eq!(next["result"]["stopReason"], "end_turn", "{context}");
                    assert_eq!(next["result"]["_meta"]["jcode"]["turnId"], next_start["params"]["turnId"], "{context}");
                    witness.wait_for_notification(is_end).await?;
                    // Close every view: settles all waits and terminates local
                    // observation brackets, even if a view has been reloaded.
                    for client in [&mut *origin, &mut *observer, &mut witness] {
                        client.request("session/close", json!({"sessionId":session})).await?;
                        initialize(client).await?;
                        check_view(&client.tape, &context);
                        assert!(client.pending.iter().all(|m| m.get("method").is_some()), "unanswered response: {context}");
                    }
                    let output: Vec<_> = origin.tape.iter().filter(|r|r["dir"]=="a2c").map(|r|r["msg"].clone()).collect();
                    let expected = if operation == "steer" { vec![before.as_str(), after.as_str(), "successor-only"] } else { vec![before.as_str(), "successor-only"] };
                    assert_eq!(texts(&output), expected, "output attribution: {context}");
                    a.shutdown().await; b.shutdown().await; witness.shutdown().await;
                    Ok::<(),anyhow::Error>(())
                }.await;
                case.with_context(|| context)?;
            }
        }
        Ok(())
    }.await;
    task.abort();
    result
}

fn queue_turn(provider: &MockProvider, text: &str) {
    provider.queue_response(vec![
        StreamEvent::TextDelta(text.into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]);
}
fn is_text(message: &Value, text: &str) -> bool {
    message["params"]["update"]["content"]["text"] == text
}
fn is_start(message: &Value) -> bool {
    message["method"] == "_jcode/turn" && message["params"]["state"] == "started"
}
fn is_end(message: &Value) -> bool {
    message["method"] == "_jcode/turn" && message["params"]["state"] != "started"
}

fn check_view(tape: &[Value], context: &str) {
    let mut active = std::collections::BTreeMap::new();
    let mut ended = std::collections::BTreeSet::new();
    for record in tape.iter().filter(|r| r["dir"] == "a2c") {
        let message = &record["msg"];
        if message["method"] != "_jcode/turn" {
            continue;
        }
        let params = &message["params"];
        let session = params["sessionId"].as_str().unwrap();
        let turn = params["turnId"].as_str().unwrap();
        if is_start(message) {
            assert!(
                active.insert(session, turn).is_none(),
                "nested lifecycle: {context}"
            );
            // Reattachment may reopen the same shared turn, as specified.
            ended.remove(&(session, turn));
        } else {
            assert_eq!(
                active.remove(session),
                Some(turn),
                "unmatched terminal: {context}"
            );
            assert!(
                ended.insert((session, turn)),
                "duplicate terminal: {context}"
            );
        }
    }
    assert!(active.is_empty(), "client stuck running: {context}");
}
