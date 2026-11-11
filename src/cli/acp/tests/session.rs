use super::*;

#[tokio::test]
async fn peer_output_is_held_until_the_attach_handshake_releases_it() {
    let gate = Arc::new(Mutex::new(PeerActionGate::default()));
    let rendered = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    {
        let mut gate = gate.lock().await;
        assert!(gate.hold(RouteAction::PeerTurnStarted {
            turn_id: "t1".to_string(),
            origin: TurnOrigin::client(Some("peer".to_string()), 3),
            user_content: Some("peer prompt".to_string()),
            prompt_already_replayed: false,
        }));
        assert!(gate.hold(delta_action("peer output")));
        assert_eq!(gate.buffered.len(), 2);
    }

    let record = |action: RouteAction| {
        let rendered = Arc::clone(&rendered);
        Box::pin(async move {
            rendered
                .lock()
                .expect("render log")
                .push(action_label(&action));
            Ok(())
        }) as futures::future::BoxFuture<'static, Result<()>>
    };
    flush_peer_actions(&gate, record).await.expect("flush");
    assert_eq!(
        rendered.lock().expect("render log").as_slice(),
        ["started t1", "delta peer output"],
        "held actions flush in arrival order"
    );

    // Once released, live output is rendered immediately.
    assert!(!gate.lock().await.hold(delta_action("live")));
}

fn action_label(action: &RouteAction) -> String {
    match action {
        RouteAction::PeerTurnStarted { turn_id, .. } => format!("started {turn_id}"),
        RouteAction::PeerTurnEvent(ServerEvent::TextDelta { text }) => format!("delta {text}"),
        RouteAction::PeerTurnEvent(other) => format!("event {other:?}"),
        RouteAction::PeerTurnEnded { turn_id, .. } => format!("ended {turn_id}"),
        RouteAction::PeerPromptReplayed { turn_id } => format!("replayed {turn_id}"),
        RouteAction::SessionState(change) => format!("state {change:?}"),
        RouteAction::Plan { .. } => "plan".to_string(),
        RouteAction::PeerSteer { turn_id, index, .. } => format!("steer {turn_id} {index}"),
        RouteAction::PeerTurnAdopted { turn_id } => format!("adopted {turn_id}"),
        RouteAction::Send(request) => format!("send {request:?}"),
        RouteAction::Wake { token, .. } => format!("wake {token:?}"),
    }
}

fn delta_action(text: &str) -> RouteAction {
    RouteAction::PeerTurnEvent(ServerEvent::TextDelta {
        text: text.to_string(),
    })
}

/// A live peer event that arrives while the handshake flush is rendering a
/// held event must render after it, never between or ahead of the buffer.
#[tokio::test]
async fn live_peer_output_renders_after_a_flush_in_progress() {
    let gate = Arc::new(Mutex::new(PeerActionGate::default()));
    {
        let mut gate = gate.lock().await;
        assert!(gate.hold(delta_action("held-1")));
        assert!(gate.hold(delta_action("held-2")));
    }
    let rendered = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

    let flush = {
        let gate = Arc::clone(&gate);
        let rendered = Arc::clone(&rendered);
        tokio::spawn(async move {
            flush_peer_actions(&gate, move |action| {
                let rendered = Arc::clone(&rendered);
                Box::pin(async move {
                    // Let the reader try to dispatch a live action now.
                    tokio::task::yield_now().await;
                    rendered
                        .lock()
                        .expect("render log")
                        .push(action_label(&action));
                    Ok(())
                })
            })
            .await
            .expect("flush succeeds")
        })
    };

    // The reader's next dispatch races the flush.
    tokio::task::yield_now().await;
    let held = gate.lock().await.hold(delta_action("live"));
    assert!(
        !held,
        "the flush already marked the gate ready, so this renders live"
    );
    rendered
        .lock()
        .expect("render log")
        .push(action_label(&delta_action("live")));

    flush.await.expect("flush task");
    assert_eq!(
        rendered.lock().expect("render log").as_slice(),
        ["delta held-1", "delta held-2", "delta live"],
        "live output renders only after every held action"
    );
}

/// Overflowing the gate during a mid-turn attach must keep the
/// `_jcode/turn` bracket: only streamed events are evicted.
#[tokio::test]
async fn overflow_keeps_the_peer_turn_bracket_balanced() {
    let gate = Arc::new(Mutex::new(PeerActionGate::default()));
    {
        let mut gate = gate.lock().await;
        assert!(gate.hold(RouteAction::PeerTurnStarted {
            turn_id: "t1".to_string(),
            origin: TurnOrigin::client(Some("peer".to_string()), 3),
            user_content: None,
            prompt_already_replayed: false,
        }));
        for index in 0..MAX_BUFFERED_PEER_ACTIONS + 16 {
            assert!(gate.hold(delta_action(&format!("chunk-{index}"))));
        }
        assert!(gate.hold(RouteAction::PeerTurnEnded {
            turn_id: "t1".to_string(),
            status: TurnEndStatus::Completed,
            message: None,
        }));
        assert_eq!(gate.buffered.len(), MAX_BUFFERED_PEER_ACTIONS);
    }

    let rendered = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    flush_peer_actions(&gate, {
        let rendered = Arc::clone(&rendered);
        move |action| {
            let rendered = Arc::clone(&rendered);
            Box::pin(async move {
                rendered
                    .lock()
                    .expect("render log")
                    .push(action_label(&action));
                Ok(())
            })
        }
    })
    .await
    .expect("flush");

    let rendered = rendered.lock().expect("render log");
    assert_eq!(
        rendered.first().map(String::as_str),
        Some("started t1"),
        "the turn's start survives overflow: {:?}",
        rendered.first()
    );
    assert_eq!(
        rendered.last().map(String::as_str),
        Some("ended t1"),
        "the turn's end survives overflow: {:?}",
        rendered.last()
    );
}

#[tokio::test]
async fn held_peer_output_is_bounded() {
    let gate = Arc::new(Mutex::new(PeerActionGate::default()));
    {
        let mut gate = gate.lock().await;
        for index in 0..MAX_BUFFERED_PEER_ACTIONS + 8 {
            assert!(gate.hold(delta_action(&format!("chunk-{index}"))));
        }
        assert_eq!(gate.buffered.len(), MAX_BUFFERED_PEER_ACTIONS);
    }
    let rendered = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    flush_peer_actions(&gate, {
        let rendered = Arc::clone(&rendered);
        move |action| {
            let rendered = Arc::clone(&rendered);
            Box::pin(async move {
                rendered
                    .lock()
                    .expect("render log")
                    .push(action_label(&action));
                Ok(())
            })
        }
    })
    .await
    .expect("flush");
    let rendered = rendered.lock().expect("render log");
    // The newest events survive: the oldest were dropped.
    assert_eq!(rendered.first().map(String::as_str), Some("delta chunk-8"));
    assert_eq!(rendered.len(), MAX_BUFFERED_PEER_ACTIONS);
}

#[test]
fn usage_updates_carry_the_session_context_gauge() {
    let state = SessionUiState {
        provider_name: Some("anthropic".to_string()),
        model: Some("claude-opus-4-6".to_string()),
        available_models: Vec::new(),
        reasoning_effort: None,
        available_reasoning_efforts: None,
        last_plan: None,
    };
    let payload = usage_update_payload("session-1", &state, 1_000, Some(4_000), None);
    assert_eq!(payload["sessionId"], "session-1");
    assert_eq!(payload["update"]["sessionUpdate"], "usage_update");
    assert_eq!(payload["update"]["size"], state.context_limit());
    assert!(
        payload["update"]["used"].as_u64().unwrap_or_default() > 0,
        "the reported input tokens count toward the gauge: {payload}"
    );
}

/// The attach replay can turn out to contain a peer turn's prompt after the
/// turn was already announced; the held announcement then drops its user
/// chunk, and only while it is still held.
#[tokio::test]
async fn a_replayed_prompt_is_dropped_from_the_held_announcement() {
    let gate = Arc::new(Mutex::new(PeerActionGate::default()));
    {
        let mut gate = gate.lock().await;
        assert!(gate.hold(RouteAction::PeerTurnStarted {
            turn_id: "gen-a:4".to_string(),
            origin: TurnOrigin::client(Some("peer".to_string()), 3),
            user_content: Some("peer prompt".to_string()),
            prompt_already_replayed: false,
        }));
        gate.mark_prompt_replayed("gen-a:9");
        assert!(
            matches!(
                gate.buffered.as_slice(),
                [RouteAction::PeerTurnStarted {
                    prompt_already_replayed: false,
                    ..
                }]
            ),
            "another turn's replay leaves the announcement alone"
        );
        gate.mark_prompt_replayed("gen-a:4");
        assert!(matches!(
            gate.buffered.as_slice(),
            [RouteAction::PeerTurnStarted {
                prompt_already_replayed: true,
                ..
            }]
        ));
    }
    flush_peer_actions(&gate, |_| Box::pin(async { Ok(()) }))
        .await
        .expect("flush");
    // Released: nothing is held any more, so there is nothing to patch.
    gate.lock().await.mark_prompt_replayed("gen-a:4");
    assert!(gate.lock().await.buffered.is_empty());
}

/// Closing a view cancels only our own running turn, scoped to it; a peer
/// turn being mirrored, or a prompt the daemon has not accepted, is left
/// alone. `session/cancel`, by contrast, targets whatever turn this view
/// shows running.
#[test]
fn closing_a_view_only_cancels_a_running_own_turn() {
    let identity = |turn_id| crate::protocol::TurnIdentity {
        runtime_generation: "gen-a".to_string(),
        turn_id,
    };
    let mut router = RouterState::new("acp-test/0", "session-1");
    router.set_turn_origin(true);
    let _prompt = router.register(77, PendingKind::Prompt);

    // A peer's turn is running and our prompt is not a turn yet.
    router.route(ServerEvent::TurnStarted {
        identity: identity(1),
        origin: Some(TurnOrigin::client(Some("other-client".to_string()), 5)),
        user_content: Some("peer prompt".to_string()),
    });
    assert!(!router.own_turn_running());
    assert_eq!(
        router.own_turn_identity(),
        None,
        "close must not cancel a peer's turn"
    );
    assert_eq!(
        router.active_turn_identity(),
        Some(identity(1)),
        "a stop in this view targets the mirrored peer turn"
    );

    router.route(ServerEvent::TurnEvent {
        identity: identity(1),
        end: Some(TurnEndStatus::Completed),
        end_message: None,
        event: Box::new(ServerEvent::Done { id: 0 }),
    });
    router.route(ServerEvent::TurnStarted {
        identity: identity(2),
        origin: Some(TurnOrigin::client(Some("acp-test/0".to_string()), 77)),
        user_content: Some("mine".to_string()),
    });
    assert!(router.own_turn_running());
    assert_eq!(
        router.own_turn_identity(),
        Some(identity(2)),
        "close cancels exactly our own running turn"
    );
    assert_eq!(router.active_turn_identity(), Some(identity(2)));
}

#[test]
fn each_connection_gets_its_own_instance_id() {
    let first = connection_instance_id("acp-process", 0);
    let second = connection_instance_id("acp-process", 1);
    assert_eq!(first, "acp-process/0");
    assert_eq!(second, "acp-process/1");
    assert_ne!(first, second);
    assert!(first.starts_with("acp-process/"));
}

fn history_message(role: &str, content: &str) -> crate::protocol::HistoryMessage {
    history_message_at(role, content, None)
}

fn history_message_at(
    role: &str,
    content: &str,
    stored_index: Option<usize>,
) -> crate::protocol::HistoryMessage {
    crate::protocol::HistoryMessage {
        response_stats: None,
        role: role.to_string(),
        content: content.to_string(),
        tool_calls: None,
        tool_data: None,
        stored_index,
    }
}

fn chunk_message_id(update: &Value) -> Option<String> {
    update["messageId"].as_str().map(str::to_string)
}

#[test]
fn streamed_messages_carry_one_stable_message_id_each() {
    let mut mapper = EventMapper::with_scope(
        "session1".to_string(),
        AcpProfile::Standard,
        "session1:own:7".to_string(),
    );

    let first = mapper.map_event(ServerEvent::TextDelta {
        text: "hello".to_string(),
    });
    let same_message = mapper.map_event(ServerEvent::TextDelta {
        text: " world".to_string(),
    });
    assert_eq!(
        chunk_message_id(&first[0]),
        Some("session1:own:7:0".to_string())
    );
    assert_eq!(
        chunk_message_id(&same_message[0]),
        chunk_message_id(&first[0]),
        "chunks of one message share its id"
    );

    // A message boundary starts the next message, so consecutive
    // same-role messages are never merged by the client.
    assert!(
        mapper
            .map_event(ServerEvent::MessageEnd {
                stop_reason: Some("tool_use".to_string()),
            })
            .is_empty()
    );
    let second = mapper.map_event(ServerEvent::TextDelta {
        text: "second message".to_string(),
    });
    assert_eq!(
        chunk_message_id(&second[0]),
        Some("session1:own:7:1".to_string())
    );
}

#[test]
fn history_replay_separates_reasoning_from_the_answer() {
    let thought = "Inspect **file** & `code` first.";
    let content = format!(
        "{}\nThe *answer* stays Markdown.",
        jcode_render_core::reasoning_line_markup(thought)
    );
    let messages = vec![history_message_at("assistant", &content, Some(9))];
    let updates = history_replay_updates("session1", &messages);
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0]["update"]["sessionUpdate"], "agent_thought_chunk");
    assert_eq!(
        updates[0]["update"]["content"]["text"],
        format!("{thought}\n")
    );
    assert_eq!(updates[1]["update"]["sessionUpdate"], "agent_message_chunk");
    assert_eq!(
        updates[1]["update"]["content"]["text"],
        "\nThe *answer* stays Markdown."
    );
    assert_ne!(
        updates[0]["update"]["messageId"],
        updates[1]["update"]["messageId"]
    );
    assert_eq!(updates[1]["update"]["messageId"], "session1:msg:9");
}

#[test]
fn history_replay_does_not_reinterpret_user_or_ordinary_markdown() {
    let user_text = jcode_render_core::reasoning_line_markup("quoted reasoning");
    let messages = vec![
        history_message("user", &user_text),
        history_message("assistant", "*ordinary italics*\n"),
    ];
    let updates = history_replay_updates("session1", &messages);
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0]["update"]["content"]["text"], user_text);
    assert_eq!(
        updates[1]["update"]["content"]["text"],
        "*ordinary italics*\n"
    );
    assert_eq!(updates[1]["update"]["sessionUpdate"], "agent_message_chunk");
}

#[test]
fn history_replay_does_not_impersonate_assistant_with_diagnostics() {
    let messages = vec![
        history_message("system", "compaction notice"),
        history_message("error", "provider failed"),
        history_message("future-role", "unknown"),
    ];
    assert!(history_replay_updates("session1", &messages).is_empty());
    let updates = history_replay_updates_with_notices("session1", &messages, true);
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0]["update"]["sessionUpdate"], "notice");
    assert_eq!(updates[0]["update"]["severity"], "info");
    assert_eq!(updates[1]["update"]["severity"], "error");
    assert_eq!(updates[1]["update"]["description"], "provider failed");
}

#[test]
fn history_replay_keeps_read_results_in_tool_blocks() {
    let mut read = history_message_at("tool", "raw file contents", Some(4));
    read.tool_data = Some(crate::message::ToolCall {
        id: "read-1".to_string(),
        name: "read".to_string(),
        input: json!({"file_path": "/workspace/file.rs"}),
        ..Default::default()
    });
    let messages = vec![
        history_message("user", "read the file"),
        read,
        history_message("assistant", "Here is my explanation."),
    ];
    let updates = history_replay_updates("session1", &messages);
    let tool = &updates[1]["update"];
    assert_eq!(tool["sessionUpdate"], "tool_call");
    assert_eq!(tool["toolCallId"], "read-1");
    assert_eq!(tool["kind"], "read");
    assert_eq!(tool["status"], "completed");
    assert_eq!(tool["rawInput"]["file_path"], "/workspace/file.rs");
    assert_eq!(tool["content"][0]["type"], "content");
    assert_eq!(tool["content"][0]["content"]["text"], "raw file contents");
    assert_eq!(tool["rawOutput"]["output"], "raw file contents");
    assert_eq!(updates[2]["update"]["sessionUpdate"], "agent_message_chunk");
    assert_eq!(history_replay_updates("session1", &messages), updates);
}

#[test]
fn history_replay_keeps_legacy_tools_out_of_chat() {
    let messages = vec![
        history_message("tool", "first result"),
        history_message("tool", "second result"),
    ];
    let updates = history_replay_updates("session1", &messages);
    for update in &updates {
        assert_eq!(update["update"]["sessionUpdate"], "tool_call");
        assert_eq!(update["update"]["name"], "tool");
    }
    assert_ne!(
        updates[0]["update"]["toolCallId"],
        updates[1]["update"]["toolCallId"]
    );
}

#[test]
fn history_replay_gives_every_message_a_distinct_stable_id() {
    let messages = vec![
        history_message("user", "first prompt"),
        history_message("assistant", "first answer"),
        history_message("assistant", "second answer"),
        history_message("user", "second prompt"),
    ];
    let updates = history_replay_updates("session1", &messages);

    let ids: Vec<String> = updates
        .iter()
        .map(|update| {
            chunk_message_id(&update["update"]).expect("every replayed message has an id")
        })
        .collect();
    assert_eq!(
        ids,
        vec![
            "session1:history:0",
            "session1:history:1",
            "session1:history:2",
            "session1:history:3",
        ],
        "ids are derived from the session and the transcript position"
    );
    assert_eq!(
        updates[0]["update"]["sessionUpdate"], "user_message_chunk",
        "user messages replay as user chunks"
    );
    assert_eq!(
        updates[1]["update"]["sessionUpdate"], "agent_message_chunk",
        "assistant messages replay as agent chunks"
    );
    assert_eq!(
        updates[2]["update"]["content"]["text"], "second answer",
        "each message keeps its own content"
    );

    // Deterministic: replaying the same transcript yields the same ids.
    assert_eq!(
        history_replay_updates("session1", &messages),
        updates,
        "replay ids are stable"
    );
}

#[test]
fn replay_ids_distinguish_prose_split_by_a_native_tool() {
    let messages = vec![
        history_message_at("assistant", "Before search", Some(7)),
        history_message_at("tool", "search results", Some(7)),
        history_message_at("assistant", "After search", Some(7)),
    ];
    let updates = history_replay_updates("session1", &messages);
    assert_eq!(updates[0]["update"]["messageId"], "session1:msg:7");
    assert_eq!(updates[2]["update"]["messageId"], "session1:msg:7:part:1");
}

#[test]
fn replay_ids_prefer_the_stored_message_index() {
    let messages = vec![
        history_message_at("user", "prompt", Some(3)),
        history_message_at("assistant", "answer", Some(7)),
        // A synthetic row (for example the compacted-history notice) has no
        // stored message, so it falls back to its position.
        history_message_at("assistant", "[compacted]", None),
    ];
    let updates = history_replay_updates("session1", &messages);
    let ids: Vec<String> = updates
        .iter()
        .map(|update| chunk_message_id(&update["update"]).expect("id"))
        .collect();
    assert_eq!(
        ids,
        vec!["session1:msg:3", "session1:msg:7", "session1:history:2",],
        "stored indices are used when present, position otherwise"
    );
}

#[test]
fn replay_ids_survive_a_shift_in_the_rendered_list() {
    // The same stored messages, but a render pass that drops a tool-only
    // round shifts their positions in `messages`. The ids must not move.
    let before = vec![
        history_message_at("user", "prompt", Some(3)),
        history_message_at("assistant", "tool round", Some(4)),
        history_message_at("assistant", "answer", Some(7)),
    ];
    let after = vec![
        history_message_at("user", "prompt", Some(3)),
        history_message_at("assistant", "answer", Some(7)),
    ];
    let ids = |messages: &[crate::protocol::HistoryMessage]| -> Vec<String> {
        history_replay_updates("session1", messages)
            .iter()
            .map(|update| chunk_message_id(&update["update"]).expect("id"))
            .collect()
    };
    let before_ids = ids(&before);
    let after_ids = ids(&after);
    assert_eq!(before_ids[0], after_ids[0]);
    assert_eq!(
        before_ids[2], after_ids[1],
        "the answer keeps its id when the tool round stops being rendered"
    );
    assert_ne!(
        before_ids[1], after_ids[1],
        "different stored messages keep different ids"
    );
}

#[test]
fn a_peer_prompt_chunk_carries_its_turn_scoped_message_id() {
    let turn_id = turn_id_string(&crate::protocol::TurnIdentity {
        runtime_generation: "gen-a".to_string(),
        turn_id: 3,
    });
    assert_eq!(turn_id, "gen-a:3", "the canonical turn id");
    let update = peer_user_message_chunk("session1", &turn_id, "peer prompt");
    assert_eq!(update["update"]["sessionUpdate"], "user_message_chunk");
    assert_eq!(
        update["update"]["messageId"], "session1:gen-a:3:user",
        "the peer prompt is one identifiable message"
    );
    assert_eq!(update["update"]["content"]["text"], "peer prompt");
}

// session/list

fn catalog_entry(
    session_id: &str,
    working_dir: Option<&str>,
    updated_at_ms: Option<i64>,
) -> jcode_harness_api_server::session_catalog::SessionCatalogEntry {
    jcode_harness_api_server::session_catalog::SessionCatalogEntry {
        session_id: session_id.to_string(),
        working_dir: working_dir.map(str::to_string),
        title: Some(format!("title {session_id}")),
        updated_at_ms,
    }
}

#[test]
fn session_list_filters_by_canonical_cwd_and_sorts_newest_first() {
    let dir = tempfile::tempdir().expect("cwd");
    let other = tempfile::tempdir().expect("other cwd");
    let entries = vec![
        catalog_entry("s-old", Some(&dir.path().display().to_string()), Some(10)),
        catalog_entry("s-new", Some(&dir.path().display().to_string()), Some(30)),
        catalog_entry(
            "s-other",
            Some(&other.path().display().to_string()),
            Some(20),
        ),
        catalog_entry("s-nowhere", None, Some(40)),
    ];

    let result = session_list_result(entries.clone(), Some(dir.path()), None, None);
    let ids: Vec<&str> = result["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .map(|session| session["sessionId"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["s-new", "s-old"]);
    assert_eq!(
        result["sessions"][0]["cwd"],
        dir.path().display().to_string()
    );
    assert_eq!(result["sessions"][0]["title"], "title s-new");
    assert!(result["sessions"][0]["updatedAt"].is_string());
    assert!(
        result.get("nextCursor").is_none(),
        "a short page has no cursor"
    );

    // Without `cwd` every session with a working directory is listed,
    // newest first (ACP requires a string cwd, so `s-nowhere` is omitted).
    let result = session_list_result(entries, None, None, None);
    let ids: Vec<&str> = result["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .map(|session| session["sessionId"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["s-new", "s-other", "s-old"]);
}

#[test]
fn session_list_omits_sessions_without_a_working_directory() {
    let entries = vec![
        catalog_entry("s-with-dir", Some("/tmp"), Some(10)),
        catalog_entry("s-no-dir", None, Some(20)),
    ];
    let result = session_list_result(entries, None, None, None);
    let ids: Vec<&str> = result["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .map(|session| session["sessionId"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["s-with-dir"]);
    assert_eq!(
        result["sessions"][0]["cwd"], "/tmp",
        "cwd is always a string"
    );
}

#[test]
fn session_list_matches_equivalent_cwd_spellings() {
    let dir = tempfile::tempdir().expect("cwd");
    let dot_spelling = format!("{}/.", dir.path().display());
    let entries = vec![
        catalog_entry("s-plain", Some(&dir.path().display().to_string()), Some(10)),
        catalog_entry("s-dotted", Some(&dot_spelling), Some(20)),
        catalog_entry("s-elsewhere", Some("/"), Some(30)),
    ];
    let result = session_list_result(entries, Some(dir.path()), None, None);
    let ids: Vec<&str> = result["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .map(|session| session["sessionId"].as_str().expect("id"))
        .collect();
    assert_eq!(
        ids,
        ["s-dotted", "s-plain"],
        "both spellings canonicalise to the requested cwd"
    );
}

#[test]
fn session_list_pages_with_an_opaque_cursor() {
    let entries: Vec<_> = (0..55)
        .map(|index| catalog_entry(&format!("s-{index:02}"), Some("/tmp"), Some(1000 - index)))
        .collect();

    let first = session_list_result(entries.clone(), None, None, None);
    let first_page = first["sessions"].as_array().expect("sessions");
    assert_eq!(first_page.len(), SESSION_LIST_PAGE);
    let cursor = first["nextCursor"]
        .as_str()
        .expect("next cursor")
        .to_string();
    assert_eq!(first_page[0]["sessionId"], "s-00");

    let cursor = SessionListCursor::decode(&cursor).expect("cursor decodes");
    let second = session_list_result(entries, None, Some(&cursor), None);
    let second_page = second["sessions"].as_array().expect("sessions");
    assert_eq!(second_page.len(), 5);
    assert_eq!(second_page[0]["sessionId"], "s-50");
    assert_eq!(second_page[4]["sessionId"], "s-54");
    assert!(
        second.get("nextCursor").is_none(),
        "last page has no cursor"
    );

    assert!(SessionListCursor::decode("not a cursor").is_err());
    assert!(SessionListCursor::decode(r#"{"sessionId":""}"#).is_err());
}

#[test]
fn session_list_reports_liveness_only_when_the_daemon_told_us() {
    let entries = vec![catalog_entry("s-live", Some("/tmp"), Some(5))];
    let without = session_list_result(entries.clone(), None, None, None);
    assert!(
        without["sessions"][0].get("_meta").is_none(),
        "unknown liveness is omitted, never guessed"
    );

    let live = vec!["s-live".to_string()];
    let with = session_list_result(entries, None, None, Some(&live));
    assert_eq!(with["sessions"][0]["_meta"]["jcode"]["live"], true);
}

#[test]
fn initialize_advertises_session_list() {
    let result = initialize_result(&json!({"protocolVersion": 1}), AcpProfile::Standard);
    assert!(
        result["agentCapabilities"]["sessionCapabilities"]["list"].is_object(),
        "clients discover session/list from the capabilities"
    );
}

#[test]
fn optional_cwd_must_be_absolute_when_present() {
    assert_eq!(optional_cwd_from_params(&json!({})).unwrap(), None);
    assert_eq!(
        optional_cwd_from_params(&json!({"cwd": "/tmp"})).unwrap(),
        Some(PathBuf::from("/tmp"))
    );
    assert!(optional_cwd_from_params(&json!({"cwd": "relative"})).is_err());
}

#[test]
fn prompts_steer_only_on_a_daemon_that_advertises_it() {
    let full = DaemonCapabilities::from_pong(&crate::protocol::server_capabilities());
    assert!(full.turn_origin && full.soft_interrupt_ids, "{full:?}");
    let old = DaemonCapabilities::from_pong(&["soft_interrupt_ids".to_string()]);
    assert!(!old.turn_origin && old.soft_interrupt_ids);

    let router_with = |capabilities: DaemonCapabilities| {
        let mut router = RouterState::new("acp-test/0", "session-1");
        router.set_turn_origin(capabilities.turn_origin);
        router.set_soft_interrupt_ids(capabilities.soft_interrupt_ids);
        router
    };

    // A steering daemon gets a soft interrupt with this connection's id.
    let mut router = router_with(full);
    let _prompt = router.register(41, PendingKind::Prompt);
    match prompt_request(&mut router, 41, "hello".into(), Vec::new(), None) {
        Request::SoftInterrupt {
            id,
            content,
            urgent,
            client_id,
            ..
        } => {
            assert_eq!((id, content.as_str(), urgent), (41, "hello", false));
            assert_eq!(client_id.as_deref(), Some("acp-test/0/p41"));
        }
        other => panic!("expected a steer, got {other:?}"),
    }

    // A sticky skill cannot ride a soft interrupt: `Message` first.
    let _skill = router.register(42, PendingKind::Prompt);
    assert!(matches!(
        prompt_request(
            &mut router,
            42,
            "x".into(),
            Vec::new(),
            Some("review".into())
        ),
        Request::Message {
            id: 42,
            active_skill: Some(_),
            ..
        }
    ));

    // Legacy daemons, or one without client ids on soft interrupts, get the
    // original `Message`.
    for capabilities in [
        DaemonCapabilities::default(),
        old,
        DaemonCapabilities {
            preserve_work: false,
            turn_origin: true,
            soft_interrupt_ids: false,
        },
    ] {
        let mut router = router_with(capabilities);
        assert!(matches!(
            prompt_request(&mut router, 43, "x".into(), Vec::new(), None),
            Request::Message { id: 43, .. }
        ));
    }
}

#[test]
fn steer_messages_carry_turn_scoped_ids() {
    let chunk = user_message_chunk("s1", "s1:gen:4:steer:0".to_string(), "also this");
    assert_eq!(chunk["update"]["sessionUpdate"], "user_message_chunk");
    assert_eq!(chunk["update"]["messageId"], "s1:gen:4:steer:0");
    assert_eq!(chunk["update"]["content"]["text"], "also this");
    assert_eq!(turn_end_state(TurnEndStatus::Cancelled), "canceled");
}

#[test]
fn detach_support_requires_work_preservation_and_withdrawal() {
    let mut router = RouterState::new("test", "session");
    router.preserve_work = true;
    assert!(!SharedControlSupport::from_router(&router).detach);
    router.set_turn_origin(true);
    assert!(!SharedControlSupport::from_router(&router).detach);
    router.set_soft_interrupt_ids(true);
    assert!(SharedControlSupport::from_router(&router).detach);
    assert_eq!(
        jcode_session_meta("test", SharedControlSupport::from_router(&router))["sharedControl"]["detach"],
        true
    );
    let legacy: Request =
        serde_json::from_value(json!({"type":"prepare_disconnect","id":1})).unwrap();
    assert!(matches!(
        legacy,
        Request::PrepareDisconnect {
            preserve_work: false,
            ..
        }
    ));
}

#[test]
fn unattributed_model_work_is_unknown_not_turnless() {
    let usage = TurnUsage::default();
    let unknown = streamed_prompt_response("end_turn", &usage, None, false);
    assert_eq!(unknown, json!({"stopReason":"end_turn"}));
    let turnless = streamed_prompt_response("cancelled", &usage, None, true);
    assert_eq!(
        turnless["_meta"]["jcode"],
        json!({"turnId":null,"role":"none"})
    );
    assert_eq!(
        prompt_response("end_turn", &usage)["_meta"]["jcode"],
        json!({"turnId":null,"role":"none"})
    );
    assert_eq!(
        streamed_prompt_response("end_turn", &usage, Some(("t1".into(), "origin")), false)["_meta"]
            ["jcode"],
        json!({"turnId":"t1","role":"origin"})
    );
}

#[cfg(unix)]
#[tokio::test]
async fn repeated_load_reuses_attachment_and_active_peer_identity() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().unwrap();
    struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => crate::env::set_var(key, value),
                    None => crate::env::remove_var(key),
                }
            }
        }
    }
    let _restore = Restore(
        ["JCODE_SOCKET", "JCODE_RUNTIME_DIR"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect(),
    );
    crate::env::set_var("JCODE_SOCKET", temp.path().join("absent.sock"));
    crate::env::set_var("JCODE_RUNTIME_DIR", temp.path());
    let runtime = AcpRuntime::new(AcpProfile::Standard, ProviderChoice::Claude, None, None);
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let (reader, writer) = client.into_split();
    let session = Arc::new(DaemonSession::new(
        "existing",
        None,
        writer,
        "observer".into(),
        1,
    ));
    session.router.lock().await.set_turn_origin(true);
    let identity = crate::protocol::TurnIdentity {
        runtime_generation: "load-test".into(),
        turn_id: 1,
    };
    session.router.lock().await.route(ServerEvent::TurnStarted {
        identity: identity.clone(),
        origin: Some(TurnOrigin::client(Some("peer".into()), 9)),
        user_content: Some("running".into()),
    });
    session.peer_gate.lock().await.ready = true;
    *session.active_skill.lock().await = Some("keep-skill".into());
    runtime
        .sessions
        .lock()
        .await
        .insert("existing".into(), session.clone());
    session.spawn_reader(reader, runtime.clone());
    let (cancel_seen, cancel_received) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(async move {
        let (reader, mut writer) = server.into_split();
        let mut reader = BufReader::new(reader);
        for _ in 0..2 {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let Request::GetHistory { id } = serde_json::from_str(&line).unwrap() else {
                panic!("repeated load must reuse the socket, not subscribe again");
            };
            let history = json!({"type":"history", "id":id,
                "session_id":"existing", "messages":[], "all_sessions":["existing"]});
            writer
                .write_all(format!("{history}\n").as_bytes())
                .await
                .unwrap();
        }
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let Request::Cancel { turn, .. } = serde_json::from_str(&line).unwrap() else {
            panic!("Stop after repeated load must still reach the daemon");
        };
        assert_eq!(turn.unwrap().turn_id, 1);
        cancel_seen.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    for _ in 0..2 {
        let loaded = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            runtime.attach_existing_session("existing".into(), PathBuf::from("/unused"), true),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(Arc::ptr_eq(&loaded, &session));
        assert_eq!(
            loaded.router.lock().await.active_turn_identity(),
            Some(identity.clone())
        );
        assert_eq!(
            loaded.active_skill.lock().await.as_deref(),
            Some("keep-skill")
        );
        assert!(loaded.peer_gate.lock().await.ready);
    }
    assert_eq!(runtime.connection_seq.load(Ordering::SeqCst), 0);
    runtime
        .handle_session_cancel(
            JsonRpcMessage::parse(
                r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"existing"}}"#,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    cancel_received.await.unwrap();
    // EOF finishes the old reader and its local bracket before reconnect.
    daemon.abort();
    let _ = daemon.await;
    let old_reader = session.reader_task.lock().unwrap().take().unwrap();
    old_reader.await.unwrap();
    let listener = tokio::net::UnixListener::bind(temp.path().join("absent.sock")).unwrap();
    let replacement = tokio::spawn(async move {
        // ensure_daemon first probes listener liveness without a handshake.
        let (probe, _) = listener.accept().await.unwrap();
        drop(probe);
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let Request::Ping { id } = serde_json::from_str(&line).unwrap() else {
            panic!("expected capability probe on replacement connection");
        };
        let pong = serde_json::to_string(&ServerEvent::Pong {
            id,
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
        let Request::Subscribe {
            id,
            target_session_id,
            ..
        } = serde_json::from_str(&line).unwrap()
        else {
            panic!("dead attachment must subscribe again");
        };
        assert_eq!(target_session_id.as_deref(), Some("existing"));
        let history = json!({"type":"history", "id":id,
            "session_id":"existing", "messages":[], "all_sessions":["existing"]});
        writer
            .write_all(format!("{history}\n{}\n", json!({"type":"done","id":id})).as_bytes())
            .await
            .unwrap();
    });
    let reconnected = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        runtime.attach_existing_session("existing".into(), temp.path().to_owned(), false),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!Arc::ptr_eq(&reconnected, &session));
    assert_eq!(runtime.connection_seq.load(Ordering::SeqCst), 1);
    replacement.await.unwrap();
}
