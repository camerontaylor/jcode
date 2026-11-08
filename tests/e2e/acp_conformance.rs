//! Wire assertions against the real stdio adapter and an isolated mock daemon.
use crate::test_support::*;
use crate::test_support::{acp::*, acp_fixture};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

async fn initialize(c: &mut AcpClient) -> Result<()> {
    let r = c
        .request("initialize", json!({"protocolVersion":1}))
        .await?;
    assert_eq!(r["protocolVersion"], 1);
    let caps = &r["agentCapabilities"];
    assert_eq!(caps["loadSession"], true);
    assert_eq!(caps["mcpCapabilities"], json!({"http":false,"sse":false}));
    assert_eq!(
        caps["promptCapabilities"],
        json!({"image":true,"audio":false,"embeddedContext":true})
    );
    for k in ["close", "resume", "list"] {
        assert!(caps["sessionCapabilities"][k].is_object());
    }
    assert_eq!(r["agentInfo"]["name"], "jcode");
    assert!(r["agentInfo"]["version"].is_string());
    Ok(())
}

#[tokio::test]
async fn acp_conformance_framing_ids_and_errors() -> Result<()> {
    let _env = setup_test_env()?;
    let mut c = AcpClient::spawn()?;
    initialize(&mut c).await?;
    c.send(json!({"jsonrpc":"2.0","id":"initialize-string","method":"initialize","params":{"protocolVersion":1}})).await?;
    let string_response = c.read_message().await?;
    assert_eq!(string_response["id"], "initialize-string");
    assert_eq!(string_response["result"]["protocolVersion"], 1);
    for (id, method, params, code) in [
        (json!("string-id"), "unknown", json!({}), -32601),
        (json!(101), "_jcode/unknown", json!({}), -32601),
        (json!(102), "session/load", json!({}), -32602),
        (
            json!(103),
            "session/new",
            json!({"cwd":std::env::current_dir()?,"mcpServers":{}}),
            -32602,
        ),
        (
            json!(104),
            "session/prompt",
            json!({"sessionId":"unknown","prompt":[]}),
            -32602,
        ),
    ] {
        c.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        let r = c.read_message().await?;
        assert_eq!(r["id"], id);
        assert_eq!(r["error"]["code"], code);
    }
    // Invalid JSON gets a null-id parse error; the next valid line still works.
    c.stdin.write_all(b"{broken\n").await?;
    c.stdin.flush().await?;
    let r = c.read_message().await?;
    assert_eq!(r["id"], Value::Null);
    assert_eq!(r["error"]["code"], -32700);
    initialize(&mut c).await?;
    c.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn acp_conformance_create_list_load() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime = tempfile::Builder::new()
        .prefix("acp-wire-")
        .tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let provider = Arc::new(MockProvider::new());
    provider.queue_response(vec![
        StreamEvent::TextDelta("fixture-answer".into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]);
    let server =
        server::Server::new_with_paths(provider, socket.clone(), runtime.path().join("debug.sock"));
    let task = tokio::spawn(async move { server.run().await });
    let result = async {
        wait_for_socket(&socket).await?;
        let mut c = AcpClient::spawn()?;
        initialize(&mut c).await?;
        let cwd = std::env::current_dir()?;
        let mut ids = Vec::new();
        for servers in [None, Some(Value::Null), Some(json!([]))] {
            let mut params = json!({"cwd":cwd});
            if let Some(v) = servers {
                params["mcpServers"] = v;
            }
            let r = c.request("session/new", params).await?;
            assert!(r["sessionId"].is_string());
            ids.push(r["sessionId"].clone());
        }
        let id = &ids[0];
        c.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}}))
            .await?;
        let r = c
            .request(
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"fixture-question"}]}),
            )
            .await?;
        assert_eq!(r["stopReason"], "end_turn");
        let updates = c.take_notifications();
        assert_eq!(texts(&updates), vec!["fixture-answer"]);
        let listed = c.request("session/list", json!({"cwd":cwd})).await?;
        assert!(
            listed["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["sessionId"] == *id && s["cwd"] == json!(cwd))
        );
        c.request("session/load", json!({"sessionId":id,"cwd":cwd}))
            .await?;
        // Replay notifications arrive before the load response.
        let replay = c.take_notifications();
        assert_eq!(texts(&replay), vec!["fixture-answer"]);
        acp_fixture::check("01-create-list-load-v1.jsonl", &c.tape)?;
        c.request("session/close", json!({"sessionId":id})).await?;
        c.request("session/close", json!({"sessionId":id})).await?;
        c.request("session/resume", json!({"sessionId":id,"cwd":cwd}))
            .await?;
        assert!(texts(&c.take_notifications()).is_empty());
        c.shutdown().await;
        Ok(())
    }
    .await;
    task.abort();
    result
}

#[tokio::test]
async fn acp_conformance_maps_reasoning_tool_calls_and_plan() -> Result<()> {
    let _env = setup_test_env()?;
    // The default `acp` tool profile has no `todo` tool, so this scenario asks
    // for exactly the two tools it scripts. Everything else about the adapter
    // is the shipped configuration.
    let _tools = EnvVarGuard::set("JCODE_TOOLS", "todo,read");
    let runtime = tempfile::Builder::new()
        .prefix("acp-mappings-")
        .tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let provider = Arc::new(MockProvider::new());
    provider.queue_response(vec![
        StreamEvent::ThinkingStart,
        StreamEvent::ThinkingDelta("weighing the plan".into()),
        StreamEvent::ThinkingEnd,
        StreamEvent::ToolUseStart {
            id: "call-todo".into(),
            name: "todo".into(),
        },
        StreamEvent::ToolInputDelta(
            json!({"todos": [{
                "name": "wire-check",
                "status": "in_progress",
                "priority": "high",
                "confidence": "plausible",
            }]})
            .to_string(),
        ),
        StreamEvent::ToolUseEnd,
        StreamEvent::ToolUseStart {
            id: "call-read".into(),
            name: "read".into(),
        },
        StreamEvent::ToolInputDelta(json!({"file_path": "Cargo.toml"}).to_string()),
        StreamEvent::ToolUseEnd,
        StreamEvent::MessageEnd {
            stop_reason: Some("tool_use".into()),
        },
    ]);
    provider.queue_response(vec![
        StreamEvent::TextDelta("all mapped".into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]);
    let server =
        server::Server::new_with_paths(provider, socket.clone(), runtime.path().join("debug.sock"));
    let task = tokio::spawn(async move { server.run().await });
    let result = async {
        wait_for_socket(&socket).await?;
        let mut c = AcpClient::spawn()?;
        initialize(&mut c).await?;
        let cwd = std::env::current_dir()?;
        let new = c.request("session/new", json!({"cwd":cwd})).await?;
        let id = &new["sessionId"];
        let answer = c
            .request(
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"map it"}]}),
            )
            .await?;
        assert_eq!(answer["stopReason"], "end_turn");
        let updates: Vec<Value> = c
            .take_notifications()
            .into_iter()
            .filter(|m| m["method"] == "session/update")
            .map(|m| m["params"]["update"].clone())
            .collect();
        let position = |predicate: &dyn Fn(&Value) -> bool| -> usize {
            updates
                .iter()
                .position(|update| predicate(update))
                .unwrap_or_else(|| panic!("missing update in {updates:#?}"))
        };
        let is_tool = |id: &'static str, kind: &'static str, status: &'static str| {
            move |update: &Value| {
                update["toolCallId"] == id
                    && update["sessionUpdate"] == kind
                    && update["status"] == status
            }
        };
        // Reasoning is exposed as standard thought chunks, before any output.
        let thought = position(&|update: &Value| update["sessionUpdate"] == "agent_thought_chunk");
        assert_eq!(updates[thought]["content"]["text"], "weighing the plan");
        // The initial call precedes every update for it, and reports the
        // daemon's programmatic name and the planning kind.
        let todo_call = position(&is_tool("call-todo", "tool_call", "pending"));
        assert_eq!(updates[todo_call]["name"], "todo");
        assert_eq!(updates[todo_call]["kind"], "think");
        let todo_running = position(&is_tool("call-todo", "tool_call_update", "in_progress"));
        let todo_done = position(&is_tool("call-todo", "tool_call_update", "completed"));
        assert!(thought < todo_call && todo_call < todo_running && todo_running < todo_done);
        // File work reports the locations the tool named, resolved against the
        // session cwd the way the daemon resolves them.
        let read_call = position(&is_tool("call-read", "tool_call", "pending"));
        assert_eq!(updates[read_call]["kind"], "read");
        assert!(updates[read_call].get("locations").is_none());
        let read_running = position(&is_tool("call-read", "tool_call_update", "in_progress"));
        assert_eq!(
            updates[read_running]["locations"],
            json!([{"path": cwd.join("Cargo.toml")}])
        );
        let read_done = position(&is_tool("call-read", "tool_call_update", "completed"));
        assert!(read_call < read_running && read_running < read_done);
        // The answer text comes after the tool frames.
        let text = position(&|update: &Value| update["sessionUpdate"] == "agent_message_chunk");
        assert_eq!(updates[text]["content"]["text"], "all mapped");
        assert!(read_done < text);
        // The todo tool's rendered card is not the plan: the adapter asks the
        // daemon for the structured snapshot. The snapshot is session state, so
        // it is written when the daemon answers the request, which is not
        // sequenced against the turn's own frames: it can land before the tool
        // completion frames or after the prompt response. Assert the content
        // and that it is sent once, not a position.
        let plan = match updates
            .iter()
            .find(|update| update["sessionUpdate"] == "plan")
        {
            Some(update) => update.clone(),
            None => c
                .wait_for_notification(|m| m["params"]["update"]["sessionUpdate"] == "plan")
                .await?["params"]["update"]
                .clone(),
        };
        assert_eq!(
            plan["entries"],
            json!([{"content": "wire-check", "priority": "high", "status": "in_progress"}])
        );
        assert!(
            updates
                .iter()
                .filter(|update| update["sessionUpdate"] == "plan")
                .count()
                <= 1,
            "the plan must not be duplicated: {updates:#?}"
        );
        c.shutdown().await;
        Ok(())
    }
    .await;
    task.abort();
    result
}

fn texts(messages: &[Value]) -> Vec<&str> {
    messages
        .iter()
        .filter(|m| m["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        .filter_map(|m| m["params"]["update"]["content"]["text"].as_str())
        .collect()
}

/// The test releases a provider call only after observing the intended wire event.
struct GatedProvider {
    gate: Arc<tokio::sync::Semaphore>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait]
impl Provider for GatedProvider {
    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let gate = self.gate.clone();
        Ok(Box::pin(stream::unfold(0, move |step| {
            let gate = gate.clone();
            async move {
                match step {
                    0 => Some((Ok(StreamEvent::TextDelta(format!("answer-{call}"))), 1)),
                    1 => {
                        gate.acquire().await.unwrap().forget();
                        Some((
                            Ok(StreamEvent::MessageEnd {
                                stop_reason: Some("end_turn".into()),
                            }),
                            2,
                        ))
                    }
                    _ => None,
                }
            }
        })))
    }
    fn name(&self) -> &str {
        "mock"
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            gate: self.gate.clone(),
            calls: self.calls.clone(),
        })
    }
}

#[tokio::test]
async fn acp_conformance_shared_observe_steer() -> Result<()> {
    shared_scenario(false).await
}

#[tokio::test]
#[ignore = "contract gap: observer close omits canceled peer bracket terminal"]
async fn acp_conformance_observer_close_bracket_contract() -> Result<()> {
    shared_scenario(true).await
}

async fn shared_scenario(check_close: bool) -> Result<()> {
    let _env = setup_test_env()?;
    let runtime = tempfile::Builder::new()
        .prefix("acp-shared-")
        .tempdir_in("/tmp")?;
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
        initialize(&mut a).await?;
        initialize(&mut b).await?;
        let cwd = std::env::current_dir()?;
        let new = a.request("session/new", json!({"cwd":cwd})).await?;
        let id = &new["sessionId"];
        b.request("session/resume", json!({"sessionId":id,"cwd":cwd}))
            .await?;
        let first = a
            .send_request(
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"first"}]}),
            )
            .await?;
        let mut observed = Vec::new();
        loop {
            let m = b.read_message().await?;
            let done = m["params"]["update"]["content"]["text"] == "answer-0";
            observed.push(m);
            if done {
                break;
            }
        }
        let start = observed
            .iter()
            .position(|m| m["method"] == "_jcode/turn" && m["params"]["state"] == "started")
            .expect("peer start before text");
        let turn = observed[start]["params"]["turnId"].clone();
        assert!(turn.is_string());
        let steer = b
            .send_request(
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"steer"}]}),
            )
            .await?;
        // Injection happens at a safe point after this provider call ends.
        // Observe queue admission through the private daemon's debug socket.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let state = debug_run_command_json(
                    runtime.path().join("debug.sock"),
                    "server:memory",
                    None,
                )
                .await?;
                if state["interrupts"]["pending_interrupts"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0
                {
                    break;
                }
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        gate.add_permits(2);
        assert_eq!(
            a.wait_for_response(first).await?["result"]["stopReason"],
            "end_turn"
        );
        assert_eq!(
            b.wait_for_response(steer).await?["result"]["stopReason"],
            "end_turn"
        );
        observed.extend(b.take_notifications());
        assert_eq!(texts(&observed), vec!["answer-0", "answer-1"]);
        let brackets: Vec<_> = observed
            .iter()
            .filter(|m| m["method"] == "_jcode/turn")
            .collect();
        assert_eq!(brackets.len(), 2);
        assert_eq!(brackets[1]["params"]["state"], "completed");
        assert_eq!(brackets[1]["params"]["turnId"], turn);
        // One file per connection: this transcript is the observer/steer view.
        acp_fixture::check("02-shared-observe-steer-v1.jsonl", &b.tape)?;
        // Closing an observer ends only its local bracket.
        let next = a
            .send_request(
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"survive observer close"}]}),
            )
            .await?;
        b.wait_for_notification(|m| m["params"]["update"]["content"]["text"] == "answer-2")
            .await?;
        b.request("session/close", json!({"sessionId":id})).await?;
        let closing = b.take_notifications();
        let close_brackets: Vec<_> = closing
            .iter()
            .filter(|m| m["method"] == "_jcode/turn")
            .collect();
        if check_close {
            assert_eq!(close_brackets.len(), 2);
            assert_eq!(close_brackets[0]["params"]["state"], "started");
            assert_eq!(close_brackets[1]["params"]["state"], "canceled");
            assert_eq!(
                close_brackets[0]["params"]["turnId"],
                close_brackets[1]["params"]["turnId"]
            );
        }
        gate.add_permits(1);
        assert_eq!(
            a.wait_for_response(next).await?["result"]["stopReason"],
            "end_turn"
        );
        // Cancel a running origin turn, then send an idle stop before its successor.
        let canceled = a
            .send_request(
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"cancel me"}]}),
            )
            .await?;
        a.wait_for_notification(|m| m["params"]["update"]["content"]["text"] == "answer-3")
            .await?;
        a.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}}))
            .await?;
        assert_eq!(
            a.wait_for_response(canceled).await?["result"]["stopReason"],
            "cancelled"
        );
        a.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}}))
            .await?;
        let successor = a
            .send_request(
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"successor"}]}),
            )
            .await?;
        a.wait_for_notification(|m| m["params"]["update"]["content"]["text"] == "answer-4")
            .await?;
        gate.add_permits(1);
        assert_eq!(
            a.wait_for_response(successor).await?["result"]["stopReason"],
            "end_turn"
        );
        a.shutdown().await;
        b.shutdown().await;
        Ok(())
    }
    .await;
    task.abort();
    result
}

#[tokio::test]
async fn acp_conformance_list_metadata_and_pagination() -> Result<()> {
    let _env = setup_test_env()?;
    let cwd = std::env::current_dir()?;
    let mut expected = std::collections::BTreeSet::new();
    for i in 0..53 {
        let id = format!("fixture-list-{i:03}");
        let mut s = Session::create_with_id(id.clone(), None, Some(format!("Title {i}")));
        s.working_dir = Some(cwd.to_string_lossy().into_owned());
        s.save_prepared()?;
        expected.insert(id);
    }
    let mut c = AcpClient::spawn()?;
    initialize(&mut c).await?;
    let mut params = json!({"cwd":cwd});
    let mut seen = std::collections::BTreeSet::new();
    let mut previous: Option<String> = None;
    loop {
        let r = c.request("session/list", params.clone()).await?;
        for entry in r["sessions"].as_array().unwrap() {
            let id = entry["sessionId"].as_str().unwrap();
            assert!(seen.insert(id.to_string()), "pagination duplicates {id}");
            assert_eq!(entry["cwd"], json!(cwd));
            assert!(entry["title"].as_str().unwrap().starts_with("Title "));
            let updated = entry["updatedAt"].as_str().unwrap().to_string();
            assert!(chrono::DateTime::parse_from_rfc3339(&updated).is_ok());
            if let Some(p) = previous {
                assert!(p >= updated, "newest first");
            }
            previous = Some(updated);
        }
        match r.get("nextCursor") {
            Some(cursor) => {
                assert!(cursor.is_string());
                assert_ne!(params.get("cursor"), Some(cursor));
                params["cursor"] = cursor.clone();
            }
            None => break,
        }
    }
    assert_eq!(seen, expected);
    c.shutdown().await;
    Ok(())
}

#[tokio::test]
#[ignore = "contract gap: standard initialize omits jcode capability metadata"]
async fn acp_conformance_capability_metadata_contract() -> Result<()> {
    let _env = setup_test_env()?;
    let mut c = AcpClient::spawn()?;
    let r = c
        .request("initialize", json!({"protocolVersion":1}))
        .await?;
    let meta = &r["agentCapabilities"]["_meta"]["jcode"];
    assert_eq!(meta["interfaceVersion"], 1);
    assert_eq!(meta["sessionMcpServers"], false);
    assert_eq!(meta["profile"], "standard");
    c.shutdown().await;
    Ok(())
}
