use super::*;
use std::path::Path;

#[cfg(unix)]
#[tokio::test]
async fn acp_new_and_resumed_sessions_advertise_server_failover() {
    for resume in [false, true] {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (reader, writer) = client.into_split();
        let daemon = tokio::spawn(async move {
            let (reader, mut writer) = server.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            // The adapter probes the daemon's capabilities before subscribing.
            reader.read_line(&mut line).await.unwrap();
            let Request::Ping { id: ping_id } = serde_json::from_str(&line).unwrap() else {
                panic!("expected the capability probe");
            };
            let pong = serde_json::to_string(&ServerEvent::Pong {
                id: ping_id,
                native_ssh_protocol: None,
                capabilities: crate::protocol::server_capabilities(),
                user_connected_session_ids: None,
            })
            .unwrap();
            writer
                .write_all(format!("{pong}\n").as_bytes())
                .await
                .unwrap();
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let request: Request = serde_json::from_str(&line).unwrap();
            let Request::Subscribe {
                id,
                supports_server_failover,
                supports_turn_identity,
                target_session_id,
                client_instance_id,
                ..
            } = request
            else {
                panic!("expected ACP subscription");
            };
            assert!(
                supports_server_failover,
                "ACP must delegate account failover to the daemon"
            );
            assert!(
                supports_turn_identity,
                "a daemon advertising turn_origin is read with turn identity"
            );
            assert!(
                client_instance_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("acp-") && id.contains('/')),
                "each connection reports its own instance id: {client_instance_id:?}"
            );
            assert_eq!(target_session_id.as_deref(), resume.then_some("existing"));
            // Stop after the subscription, without needing a live provider or daemon.
            let response = json!({"type": "error", "id": id, "message": "subscription captured"});
            writer
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        });
        let runtime = AcpRuntime::new(AcpProfile::Standard, ProviderChoice::Claude, None, None);
        let cwd = PathBuf::from("/acp-test");
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            if resume {
                runtime
                    .attach_existing_session_connected(
                        "existing".into(),
                        cwd,
                        false,
                        reader,
                        writer,
                    )
                    .await
            } else {
                runtime
                    .create_new_session_connected(cwd, reader, writer)
                    .await
            }
        })
        .await
        .expect("ACP subscription timed out");
        daemon.await.unwrap();
        assert_eq!(
            result
                .err()
                .expect("mock daemon rejects subscription")
                .to_string(),
            "subscription captured"
        );
    }
}

#[test]
fn acp_tool_kind_maps_core_tools() {
    assert_eq!(tool_kind("read"), "read");
    assert_eq!(tool_kind("apply_patch"), "edit");
    assert_eq!(tool_kind("bash"), "execute");
    assert_eq!(tool_kind("agentgrep"), "search");
    assert_eq!(tool_kind("webfetch"), "fetch");
    assert_eq!(tool_kind("todo"), "think");
    assert_eq!(tool_kind("swarm"), "other");
    // Names are matched against the table, never inferred from their spelling:
    // a tool this build does not know stays `other` even when its name sounds
    // like a known kind.
    assert_eq!(tool_kind("delete_file"), "other");
    assert_eq!(tool_kind("move_file"), "other");
    assert_eq!(tool_kind("read_file"), "other");
}

#[test]
fn json_rpc_parse_errors_use_standard_codes() {
    let (code, _) = JsonRpcMessage::parse("not json").unwrap_err();
    assert_eq!(code, JSONRPC_PARSE_ERROR);

    let (code, message) = JsonRpcMessage::parse(r#"{"method":"initialize"}"#).unwrap_err();
    assert_eq!(code, JSONRPC_INVALID_REQUEST);
    assert!(message.contains("jsonrpc"));
}

#[test]
fn prompt_from_params_accepts_text_images_and_resources() {
    let params = json!({
        "sessionId": "s1",
        "prompt": [
            {"type": "text", "text": "hello"},
            {"type": "image", "mimeType": "image/png", "data": "abc"},
            {"type": "resource", "resource": {"uri": "file:///tmp/a.rs", "text": "fn main(){}"}},
            {"type": "resource_link", "uri": "file:///tmp/b.rs", "name": "b.rs"}
        ]
    });
    let (text, images) = prompt_from_params(&params).unwrap();
    assert!(text.contains("hello"));
    assert!(text.contains("Embedded resource: file:///tmp/a.rs"));
    assert!(text.contains("Resource link: b.rs"));
    assert_eq!(images, vec![("image/png".to_string(), "abc".to_string())]);
}

#[test]
fn prompt_usage_deduplicates_live_snapshots_and_resets_per_call() {
    let mut usage = TurnUsage::default();
    usage.observe_snapshot(10, 2, Some(4), None);
    usage.observe_snapshot(10, 3, Some(4), Some(5));
    usage.observe_snapshot(10, 3, Some(4), Some(5));
    // Stale or partial snapshots must not lower the per-call watermark.
    usage.observe_snapshot(8, 1, Some(2), None);
    usage.observe_snapshot(10, 3, Some(4), Some(5));
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 3);
    assert_eq!(usage.cached_read_tokens, Some(4));
    assert_eq!(usage.cached_write_tokens, Some(5));
    usage.begin_call();
    usage.observe_snapshot(20, 3, Some(6), Some(7));
    assert_eq!(usage.input_tokens, 30);
    assert_eq!(usage.output_tokens, 6);
    assert_eq!(usage.cached_read_tokens, Some(10));
    assert_eq!(usage.cached_write_tokens, Some(12));
}

#[test]
fn prompt_response_reports_usage_accumulated_across_the_turn() {
    let mut usage = TurnUsage::default();
    usage.add(10, 2, Some(4), Some(5));
    usage.add(20, 3, Some(6), Some(7));

    assert_eq!(
        prompt_response("end_turn", &usage),
        json!({
            "stopReason": "end_turn",
            "_meta": {"jcode": {"turnId": null, "role": "none"}},
            "usage": {
                "totalTokens": 57,
                "inputTokens": 30,
                "outputTokens": 5,
                "cachedReadTokens": 10,
                "cachedWriteTokens": 12,
            }
        })
    );
}

#[test]
fn prompt_response_omits_unreported_usage_and_cache_fields() {
    assert_eq!(
        prompt_response("end_turn", &TurnUsage::default()),
        json!({ "stopReason": "end_turn", "_meta": {"jcode": {"turnId": null, "role": "none"}} })
    );

    let mut usage = TurnUsage::default();
    usage.add(10, 2, None, None);
    assert_eq!(
        prompt_response("cancelled", &usage),
        json!({
            "stopReason": "cancelled",
            "_meta": {"jcode": {"turnId": null, "role": "none"}},
            "usage": {
                "totalTokens": 12,
                "inputTokens": 10,
                "outputTokens": 2,
            }
        })
    );
}

#[test]
fn initialize_advertises_jcode_capability_meta_in_every_profile() {
    for (profile, name, extensions) in [
        (
            AcpProfile::Standard,
            "standard",
            vec![
                "shared_control",
                "peer_turns",
                "turn_correlation",
                "skill_selection",
            ],
        ),
        (
            AcpProfile::Extended,
            "extended",
            vec![
                "shared_control",
                "peer_turns",
                "turn_correlation",
                "skill_selection",
                "raw_server_event",
            ],
        ),
        (
            AcpProfile::Full,
            "full",
            vec![
                "shared_control",
                "peer_turns",
                "turn_correlation",
                "skill_selection",
                "raw_server_event",
            ],
        ),
    ] {
        let result = initialize_result(&json!({"protocolVersion": 1}), profile);
        let meta = &result["agentCapabilities"]["_meta"]["jcode"];
        assert_eq!(
            meta,
            &json!({
                "interfaceVersion": 1,
                "profile": name,
                "extensions": extensions,
                "sessionMcpServers": false,
            }),
            "{name} advertises the contract's capability metadata"
        );
        assert!(
            meta["interfaceVersion"].is_u64(),
            "interfaceVersion is a JSON number, not a string: {meta}"
        );
        let advertised: Vec<&str> = meta["extensions"]
            .as_array()
            .expect("extensions is an array")
            .iter()
            .map(|extension| extension.as_str().expect("extension names are strings"))
            .collect();
        for honored in ["turn_correlation", "skill_selection"] {
            assert!(
                advertised.contains(&honored),
                "{honored} is honored by this build, so it is advertised: {advertised:?}"
            );
        }
        assert_eq!(
            result["agentCapabilities"]["mcpCapabilities"],
            json!({"http": false, "sse": false}),
            "stdio MCP support is still declared through the metadata, not here"
        );
    }
}

#[test]
fn initialize_meta_ignores_unknown_and_absent_params() {
    for params in [
        json!({}),
        json!(null),
        json!({"protocolVersion": 1}),
        json!({"protocolVersion": 999, "unknownField": {"nested": true}}),
    ] {
        let result = initialize_result(&params, AcpProfile::Standard);
        assert_eq!(
            result["agentCapabilities"]["_meta"]["jcode"],
            json!({
                "interfaceVersion": 1,
                "profile": "standard",
                "extensions": ["shared_control", "peer_turns", "turn_correlation", "skill_selection"],
                "sessionMcpServers": false,
            }),
            "initialize params never change the advertised metadata: {params}"
        );
        assert_eq!(result["protocolVersion"], 1);
        assert_eq!(result["agentCapabilities"]["loadSession"], true);
    }
}

#[test]
fn session_meta_reports_the_attachment_and_effective_shared_control() {
    let support = SharedControlSupport {
        detach: false,
        observe: true,
        stop_observed_turn: true,
        implicit_steering: true,
    };
    assert_eq!(
        jcode_session_meta("acp-test/0", support),
        json!({
            "clientInstanceId": "acp-test/0",
            "sharedControl": {
                "observe": true,
                "stopObservedTurn": true,
                "detach": false,
                "implicitSteering": true,
            },
        })
    );

    // Without negotiated daemon preservation, detach remains false even
    // where the other shared-control behaviors are available.
    let legacy = SharedControlSupport {
        detach: false,
        observe: false,
        stop_observed_turn: false,
        implicit_steering: false,
    };
    let meta = jcode_session_meta("acp-test/1", legacy);
    assert_eq!(
        meta["sharedControl"],
        json!({
            "observe": false,
            "stopObservedTurn": false,
            "detach": false,
            "implicitSteering": false,
        })
    );
    assert_eq!(meta["clientInstanceId"], "acp-test/1");
}

#[test]
fn session_meta_merges_without_clobbering_existing_meta() {
    let support = SharedControlSupport {
        detach: false,
        observe: true,
        stop_observed_turn: true,
        implicit_steering: true,
    };

    // new/load results carry no `_meta` today: the metadata creates it.
    let mut plain = json!({"sessionId": "s1"});
    insert_jcode_session_meta(&mut plain, "acp-test/0", support);
    assert_eq!(plain["sessionId"], "s1");
    assert_eq!(plain["_meta"]["jcode"]["clientInstanceId"], "acp-test/0");

    // An existing `_meta` keeps its other keys.
    let mut existing = json!({"_meta": {"other": {"kept": true}}});
    insert_jcode_session_meta(&mut existing, "acp-test/1", support);
    assert_eq!(existing["_meta"]["other"], json!({"kept": true}));
    assert_eq!(existing["_meta"]["jcode"]["clientInstanceId"], "acp-test/1");

    // A `_meta` that is not an object is replaced rather than dropped.
    let mut odd = json!({"_meta": "not an object"});
    insert_jcode_session_meta(&mut odd, "acp-test/2", support);
    assert_eq!(odd["_meta"]["jcode"]["clientInstanceId"], "acp-test/2");
}

#[test]
fn shared_control_follows_the_daemon_not_the_adapter() {
    let router = |turn_origin: bool, soft_interrupt_ids: bool| {
        let mut router = RouterState::new("acp-test/0", "session-1");
        router.set_turn_origin(turn_origin);
        router.set_soft_interrupt_ids(soft_interrupt_ids);
        router
    };

    // A legacy daemon is read flat: peer turns are invisible and prompts are
    // not steered.
    assert_eq!(
        SharedControlSupport::from_router(&router(false, false)),
        SharedControlSupport {
            detach: false,
            observe: false,
            stop_observed_turn: false,
            implicit_steering: false,
        }
    );

    // Attributed turns alone: peers are observable and stoppable, but a prompt
    // still cannot join a running turn.
    assert_eq!(
        SharedControlSupport::from_router(&router(true, false)),
        SharedControlSupport {
            detach: false,
            observe: true,
            stop_observed_turn: true,
            implicit_steering: false,
        }
    );

    // Client ids on soft interrupts without turn identity is not steering.
    assert_eq!(
        SharedControlSupport::from_router(&router(false, true)),
        SharedControlSupport {
            detach: false,
            observe: false,
            stop_observed_turn: false,
            implicit_steering: false,
        }
    );

    // Both: the full baseline.
    assert_eq!(
        SharedControlSupport::from_router(&router(true, true)),
        SharedControlSupport {
            detach: false,
            observe: true,
            stop_observed_turn: true,
            implicit_steering: true,
        }
    );
}

#[test]
fn event_mapper_maps_tool_lifecycle() {
    let mut mapper = EventMapper::new("session1".to_string(), AcpProfile::Standard);
    let start = mapper.map_event(ServerEvent::ToolStart {
        id: "tool1".to_string(),
        name: "bash".to_string(),
    });
    assert_eq!(start[0]["sessionUpdate"], "tool_call");
    assert_eq!(start[0]["kind"], "execute");

    let input = mapper.map_event(ServerEvent::ToolInput {
        id: None,
        delta: "{\"command\":\"true\"}".to_string(),
    });
    assert_eq!(input[0]["rawInput"]["command"], "true");

    let done = mapper.map_event(ServerEvent::ToolDone {
        id: "tool1".to_string(),
        name: "bash".to_string(),
        output: "ok".to_string(),
        error: None,
    });
    assert_eq!(done[0]["status"], "completed");
    assert_eq!(done[0]["content"][0]["content"]["text"], "ok");
}

#[test]
fn event_mapper_keeps_overlapping_tool_inputs_on_their_call() {
    let mut mapper = EventMapper::new("session1".to_string(), AcpProfile::Standard);
    for id in ["a", "b"] {
        mapper.map_event(ServerEvent::ToolStart {
            id: id.to_string(),
            name: "bash".to_string(),
        });
        assert!(
            mapper
                .map_event(ServerEvent::ToolInput {
                    id: Some(id.to_string()),
                    delta: "{\"command\":".to_string(),
                })
                .is_empty()
        );
    }
    for (id, command) in [("a", "true"), ("b", "false")] {
        let updates = mapper.map_event(ServerEvent::ToolInput {
            id: Some(id.to_string()),
            delta: format!("\"{command}\"}}"),
        });
        assert_eq!(
            updates,
            vec![json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": id,
                "rawInput": {"command": command},
            })]
        );
    }
    for (id, command) in [("a", "true"), ("b", "false")] {
        let updates = mapper.map_event(ServerEvent::ToolExec {
            id: id.to_string(),
            name: "bash".to_string(),
        });
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0]["toolCallId"], id);
        assert_eq!(updates[0]["status"], "in_progress");
        assert_eq!(updates[0]["rawInput"], json!({"command": command}));
    }
}

#[test]
fn event_mapper_suppresses_incomplete_legacy_tool_input() {
    let mut mapper = EventMapper::new("session1".to_string(), AcpProfile::Standard);
    assert!(
        mapper
            .map_event(ServerEvent::ToolInput {
                id: None,
                delta: "ignored without a tool".to_string(),
            })
            .is_empty()
    );
    mapper.map_event(ServerEvent::ToolStart {
        id: "legacy".to_string(),
        name: "bash".to_string(),
    });
    for delta in ["", "{", "\"command\":"] {
        assert!(
            mapper
                .map_event(ServerEvent::ToolInput {
                    id: None,
                    delta: delta.to_string(),
                })
                .is_empty()
        );
    }
    let updates = mapper.map_event(ServerEvent::ToolInput {
        id: None,
        delta: "\"true\"}".to_string(),
    });
    assert_eq!(
        updates,
        vec![json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "legacy",
            "rawInput": {"command": "true"},
        })]
    );
}

#[test]
fn event_mapper_streams_reasoning_as_thought_chunks() {
    let mut mapper = EventMapper::new("session1".to_string(), AcpProfile::Standard);
    let first = mapper.map_event(ServerEvent::ReasoningDelta {
        text: "checking the adapter".to_string(),
    });
    assert_eq!(first[0]["sessionUpdate"], "agent_thought_chunk");
    assert_eq!(
        first[0]["content"],
        json!({"type": "text", "text": "checking the adapter"})
    );
    let second = mapper.map_event(ServerEvent::ReasoningDelta {
        text: " before answering".to_string(),
    });
    // Chunks of one reasoning block group under one message id.
    assert_eq!(first[0]["messageId"], second[0]["messageId"]);
    // The block boundary itself carries no content.
    assert!(
        mapper
            .map_event(ServerEvent::ReasoningDone {
                duration_secs: Some(1.5)
            })
            .is_empty()
    );
    let next = mapper.map_event(ServerEvent::ReasoningDelta {
        text: "a new step".to_string(),
    });
    assert_ne!(next[0]["messageId"], first[0]["messageId"]);
    // Reasoning and assistant text never share an id, even in one step.
    let answer = mapper.map_event(ServerEvent::TextDelta {
        text: "answer".to_string(),
    });
    assert_eq!(answer[0]["sessionUpdate"], "agent_message_chunk");
    assert_ne!(answer[0]["messageId"], next[0]["messageId"]);
}

#[test]
fn event_mapper_emits_no_thought_chunk_without_daemon_reasoning() {
    let mut mapper = EventMapper::new("session1".to_string(), AcpProfile::Standard);
    let updates = mapper.map_event(ServerEvent::TextDelta {
        text: "answer".to_string(),
    });
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["sessionUpdate"], "agent_message_chunk");
    // Nothing is invented when the daemon exposes no reasoning.
    assert!(
        mapper
            .map_event(ServerEvent::ReasoningDone {
                duration_secs: None
            })
            .is_empty()
    );
}

#[test]
fn event_mapper_reports_tool_locations_from_structured_input() {
    let mut mapper = EventMapper::new("session1".to_string(), AcpProfile::Standard)
        .with_working_dir(Some(PathBuf::from("/repo")));
    let start = mapper.map_event(ServerEvent::ToolStart {
        id: "tool1".to_string(),
        name: "read".to_string(),
    });
    assert_eq!(start[0]["name"], "read");
    // The streamed input is not complete yet: no location key at all, not an
    // empty array, and nothing guessed from the title.
    assert!(start[0].get("locations").is_none());
    mapper.map_event(ServerEvent::ToolInput {
        id: Some("tool1".to_string()),
        delta: "{\"file_path\":\"src/a.rs\",\"start_line\":12}".to_string(),
    });
    let exec = mapper.map_event(ServerEvent::ToolExec {
        id: "tool1".to_string(),
        name: "read".to_string(),
    });
    assert_eq!(
        exec[0]["locations"],
        json!([{"path": "/repo/src/a.rs", "line": 12}])
    );
}

#[test]
fn event_mapper_omits_locations_when_the_tool_names_no_file() {
    let mut mapper = EventMapper::new("session1".to_string(), AcpProfile::Standard)
        .with_working_dir(Some(PathBuf::from("/repo")));
    mapper.map_event(ServerEvent::ToolStart {
        id: "tool1".to_string(),
        name: "bash".to_string(),
    });
    mapper.map_event(ServerEvent::ToolInput {
        id: Some("tool1".to_string()),
        delta: "{\"command\":\"cargo test\"}".to_string(),
    });
    let exec = mapper.map_event(ServerEvent::ToolExec {
        id: "tool1".to_string(),
        name: "bash".to_string(),
    });
    assert!(exec[0].get("locations").is_none());
    let done = mapper.map_event(ServerEvent::ToolDone {
        id: "tool1".to_string(),
        name: "bash".to_string(),
        output: "ok".to_string(),
        error: None,
    });
    assert_eq!(done[0]["content"].as_array().unwrap().len(), 1);
    assert_eq!(done[0]["content"][0]["type"], "content");
}

#[test]
fn event_mapper_reports_edit_diffs_from_structured_edits() {
    let mut mapper = EventMapper::new("session1".to_string(), AcpProfile::Standard)
        .with_working_dir(Some(PathBuf::from("/repo")));
    mapper.map_event(ServerEvent::ToolStart {
        id: "tool1".to_string(),
        name: "edit".to_string(),
    });
    mapper.map_event(ServerEvent::ToolInput {
        id: Some("tool1".to_string()),
        delta: json!({
            "file_path": "src/a.rs",
            "edits": [
                {"old_string": "let a = 1;", "new_string": "let a = 2;"},
                {"old_string": "let b = 1;", "new_string": "let b = 2;"},
            ],
        })
        .to_string(),
    });
    let exec = mapper.map_event(ServerEvent::ToolExec {
        id: "tool1".to_string(),
        name: "edit".to_string(),
    });
    assert_eq!(exec[0]["locations"], json!([{"path": "/repo/src/a.rs"}]));
    let done = mapper.map_event(ServerEvent::ToolDone {
        id: "tool1".to_string(),
        name: "edit".to_string(),
        output: "Applied 2 edits".to_string(),
        error: None,
    });
    assert_eq!(
        done[0]["content"],
        json!([
            {
                "type": "diff",
                "path": "/repo/src/a.rs",
                "oldText": "let a = 1;",
                "newText": "let a = 2;",
            },
            {
                "type": "diff",
                "path": "/repo/src/a.rs",
                "oldText": "let b = 1;",
                "newText": "let b = 2;",
            },
            {"type": "content", "content": {"type": "text", "text": "Applied 2 edits"}},
        ])
    );
}

#[test]
fn event_mapper_reports_write_as_a_diff_without_old_text() {
    let mut mapper = EventMapper::new("session1".to_string(), AcpProfile::Standard)
        .with_working_dir(Some(PathBuf::from("/repo")));
    mapper.map_event(ServerEvent::ToolStart {
        id: "tool1".to_string(),
        name: "write".to_string(),
    });
    mapper.map_event(ServerEvent::ToolInput {
        id: Some("tool1".to_string()),
        delta: json!({"file_path": "src/new.rs", "content": "fn main() {}\n"}).to_string(),
    });
    let done = mapper.map_event(ServerEvent::ToolDone {
        id: "tool1".to_string(),
        name: "write".to_string(),
        output: "Wrote src/new.rs".to_string(),
        error: None,
    });
    // The previous content is unknown and is not read to invent it.
    assert_eq!(
        done[0]["content"][0],
        json!({
            "type": "diff",
            "path": "/repo/src/new.rs",
            "newText": "fn main() {}\n",
        })
    );
    assert!(done[0]["content"][0].get("oldText").is_none());
}

#[test]
fn event_mapper_reports_no_diff_for_a_failed_edit() {
    let mut mapper = EventMapper::new("session1".to_string(), AcpProfile::Standard)
        .with_working_dir(Some(PathBuf::from("/repo")));
    mapper.map_event(ServerEvent::ToolStart {
        id: "tool1".to_string(),
        name: "edit".to_string(),
    });
    mapper.map_event(ServerEvent::ToolInput {
        id: Some("tool1".to_string()),
        delta: json!({
            "file_path": "src/a.rs",
            "edits": [{"old_string": "a", "new_string": "b"}],
        })
        .to_string(),
    });
    let done = mapper.map_event(ServerEvent::ToolDone {
        id: "tool1".to_string(),
        name: "edit".to_string(),
        output: "old_string not found".to_string(),
        error: Some("old_string not found".to_string()),
    });
    assert_eq!(done[0]["status"], "failed");
    assert_eq!(done[0]["content"].as_array().unwrap().len(), 1);
    assert_eq!(done[0]["content"][0]["type"], "content");
}

fn todo_snapshot(todos: Value) -> String {
    json!({"todos": todos, "plan": {}, "goals": []}).to_string()
}

#[test]
fn plan_update_replaces_the_whole_plan_each_time() {
    let mut last = None;
    let first = plan_update(
        &mut last,
        &todo_snapshot(json!([
            {"content": "Audit the adapter", "status": "completed", "priority": "high"},
            {"content": "Map reasoning", "status": "in_progress", "priority": "medium"},
            {"content": "Map plans", "status": "pending", "priority": "low"},
        ])),
    )
    .expect("first plan");
    assert_eq!(first["sessionUpdate"], "plan");
    assert_eq!(
        first["entries"],
        json!([
            {"content": "Audit the adapter", "priority": "high", "status": "completed"},
            {"content": "Map reasoning", "priority": "medium", "status": "in_progress"},
            {"content": "Map plans", "priority": "low", "status": "pending"},
        ])
    );
    // Every update carries the whole list, never a delta of it.
    let second = plan_update(
        &mut last,
        &todo_snapshot(json!([
            {"content": "Map plans", "status": "in_progress", "priority": "low"},
        ])),
    )
    .expect("replacement plan");
    assert_eq!(
        second["entries"],
        json!([{"content": "Map plans", "priority": "low", "status": "in_progress"}])
    );
    // An unchanged snapshot is not re-sent as if it were a new plan.
    assert!(
        plan_update(
            &mut last,
            &todo_snapshot(json!([
                {"content": "Map plans", "status": "in_progress", "priority": "low"},
            ])),
        )
        .is_none()
    );
}
