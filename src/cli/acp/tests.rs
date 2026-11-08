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
    assert_eq!(tool_kind("swarm"), "other");
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
fn non_empty_mcp_servers_are_tolerated_until_session_scoped_mcp_is_supported() {
    let params = json!({"mcpServers": [{"name": "fs"}]});
    assert!(validate_acp_mcp_servers(&params).is_ok());

    let params = json!({"mcpServers": []});
    assert!(validate_acp_mcp_servers(&params).is_ok());

    for params in [json!({}), json!({"mcpServers": null})] {
        assert!(validate_acp_mcp_servers(&params).is_ok(), "{params}");
    }
    // A non-array shape stays a parameter error, and reports no notice.
    for params in [
        json!({"mcpServers": {"name": "fs"}}),
        json!({"mcpServers": "fs"}),
    ] {
        assert!(validate_acp_mcp_servers(&params).is_err(), "{params}");
        for advertised in [true, false] {
            assert_eq!(
                ignored_mcp_servers_notice("s1", &params, advertised),
                None,
                "{params} (notices advertised: {advertised})"
            );
        }
    }
}

#[test]
fn ignored_mcp_servers_produce_exactly_one_notice_naming_them() {
    let params = json!({
        "mcpServers": [
            {"name": "fs", "command": "npx", "args": ["-y", "fs"]},
            {"name": "git", "url": "https://example.test/mcp", "type": "http"}
        ]
    });
    let notice =
        ignored_mcp_servers_notice("s1", &params, true).expect("a nonempty array is reported");
    assert_eq!(
        notice,
        json!({
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "notice",
                "severity": "info",
                "title": "Client MCP servers not connected",
                "description": "This session ignores the ACP mcpServers it was given (fs, git): jcode runs the MCP servers configured daemon-side, not per session.",
            }
        }),
        "a notice is informational, names the ignored servers and points at daemon-side MCP"
    );

    // Nothing to report: an empty array, a missing field or an explicit null.
    for params in [
        json!({"mcpServers": []}),
        json!({}),
        json!({"mcpServers": null}),
    ] {
        assert_eq!(
            ignored_mcp_servers_notice("s1", &params, true),
            None,
            "{params}"
        );
    }

    // Unnamed entries are still reported, by count.
    let notice = ignored_mcp_servers_notice("s1", &json!({"mcpServers": [{}, {}]}), true)
        .expect("a nonempty array is reported");
    assert!(
        notice["update"]["description"]
            .as_str()
            .unwrap_or_default()
            .contains("2 client MCP server(s)"),
        "{notice}"
    );
}

#[test]
fn a_notice_is_only_sent_to_a_client_that_advertised_session_notices() {
    let params = json!({"mcpServers": [{"name": "fs", "command": "npx"}]});
    assert!(
        ignored_mcp_servers_notice("s1", &params, true).is_some(),
        "an advertising client gets the notice"
    );
    assert_eq!(
        ignored_mcp_servers_notice("s1", &params, false),
        None,
        "v1 forbids a notice to a client that did not advertise `session.notices`, \
         and a message is not a substitute"
    );

    // The capability is an object marker under `clientCapabilities.session`.
    for (spec, advertised) in [
        (
            json!({"clientCapabilities": {"session": {"notices": {}}}}),
            true,
        ),
        (
            json!({"clientCapabilities": {"session": {"notices": {"custom": true}}}}),
            true,
        ),
        (json!({"clientCapabilities": {"session": {}}}), false),
        (
            json!({"clientCapabilities": {"session": {"notices": null}}}),
            false,
        ),
        (json!({"clientCapabilities": {}}), false),
        (json!({"clientCapabilities": null}), false),
        (json!({}), false),
    ] {
        assert_eq!(
            client_advertises_session_notices(&spec),
            advertised,
            "{spec}"
        );
    }
}

#[test]
fn advertised_commands_cover_all_acp_daemon_model_controls() {
    let commands = acp_available_commands();
    let names: Vec<&str> = commands
        .iter()
        .map(|command| command["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["model", "models", "effort", "compact", "subagent"]);
    assert!(commands[3].get("input").is_none());
    assert!(
        commands[4]["input"]["hint"]
            .as_str()
            .unwrap()
            .contains("<prompt>")
    );
    assert_eq!(commands[0]["input"]["hint"], "model id (optional)");
    assert!(commands[1].get("input").is_none());
    assert!(
        commands[2]["input"]["hint"]
            .as_str()
            .unwrap()
            .contains("high")
    );
}

#[test]
fn advertised_commands_parse_to_real_dispatch_variants() {
    assert_eq!(
        parse_acp_slash_command("/model claude-sonnet-4-5")
            .unwrap()
            .unwrap(),
        AcpSlashCommand::Model(Some("claude-sonnet-4-5".to_string()))
    );
    assert_eq!(
        parse_acp_slash_command("/model ").unwrap().unwrap(),
        AcpSlashCommand::Model(None)
    );
    assert_eq!(
        parse_acp_slash_command("/models").unwrap().unwrap(),
        AcpSlashCommand::Models
    );
    assert_eq!(
        parse_acp_slash_command("/effort xhigh").unwrap().unwrap(),
        AcpSlashCommand::Effort(Some("xhigh".to_string()))
    );
    assert!(parse_acp_slash_command("/models now").unwrap().is_err());
    assert_eq!(
        parse_acp_slash_command("/compact").unwrap().unwrap(),
        AcpSlashCommand::Compact
    );
    assert!(parse_acp_slash_command("/compact now").unwrap().is_err());
    assert_eq!(
        parse_acp_slash_command("/subagent --model gpt-6.1-sol --type review check the -- flags")
            .unwrap()
            .unwrap(),
        AcpSlashCommand::Subagent(AcpSubagentSpec {
            prompt: "check the -- flags".to_string(),
            subagent_type: "review".to_string(),
            model: Some("gpt-6.1-sol".to_string()),
        })
    );
    assert_eq!(
        parse_acp_slash_command("/subagent fix the build")
            .unwrap()
            .unwrap(),
        AcpSlashCommand::Subagent(AcpSubagentSpec {
            prompt: "fix the build".to_string(),
            subagent_type: "general".to_string(),
            model: None,
        })
    );
    assert!(parse_acp_slash_command("/subagent").unwrap().is_err());
    assert!(
        parse_acp_slash_command("/subagent --model")
            .unwrap()
            .is_err()
    );
    assert!(
        parse_acp_slash_command("/subagent --bogus x")
            .unwrap()
            .is_err()
    );
    assert!(parse_acp_slash_command("/not-advertised").is_none());
    assert!(parse_acp_slash_command(" /model literal").is_none());
    assert!(parse_acp_slash_command("ordinary prompt").is_none());
}

fn project_skills(names: &[&str]) -> (tempfile::TempDir, crate::skill::SkillRegistry) {
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        let skill_dir = dir.path().join(".jcode/skills").join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Test {name}\n---\nDo {name}.\n"),
        )
        .unwrap();
    }
    let registry = crate::skill::SkillRegistry::load_project_overlay(Some(dir.path())).unwrap();
    (dir, registry)
}

#[test]
fn slash_and_dollar_skill_invocations_resolve_with_trailing_prompt() {
    let (_dir, skills) = project_skills(&["interrogate"]);
    assert_eq!(
        resolve_acp_skill_invocation(&skills, "/interrogate"),
        Some(("interrogate".to_string(), None))
    );
    assert_eq!(
        resolve_acp_skill_invocation(&skills, "/interrogate the auth flow\n"),
        Some(("interrogate".to_string(), Some("the auth flow".to_string())))
    );
    assert_eq!(
        resolve_acp_skill_invocation(&skills, "$interrogate why"),
        Some(("interrogate".to_string(), Some("why".to_string())))
    );
}

#[test]
fn unknown_escaped_and_path_like_slash_text_is_untouched() {
    let (_dir, skills) = project_skills(&["interrogate"]);
    for text in [
        "/unknown do things",
        "/home/user/file.rs explain",
        " /interrogate escaped",
        "please /interrogate",
        "$HOME is set",
        "ordinary prompt",
    ] {
        assert_eq!(resolve_acp_skill_invocation(&skills, text), None, "{text}");
    }
}

#[test]
fn skill_selection_reads_the_canonical_metadata_field() {
    // Canonical string selects; hosts that serialize the textual form are
    // still understood.
    assert_eq!(
        skill_selection_from_params(&json!({"_meta": {"jcode": {"skill": "/interrogate"}}})),
        Ok(SkillSelection::Select("interrogate".to_string()))
    );
    assert_eq!(
        skill_selection_from_params(&json!({"_meta": {"jcode": {"skill": "interrogate"}}})),
        Ok(SkillSelection::Select("interrogate".to_string()))
    );
    // An explicit null is the clear verb, not an absent key.
    assert_eq!(
        skill_selection_from_params(&json!({"_meta": {"jcode": {"skill": null}}})),
        Ok(SkillSelection::Clear)
    );
    // A missing key (or no `_meta` at all) leaves the selection untouched.
    assert_eq!(
        skill_selection_from_params(&json!({"_meta": {"jcode": {}}})),
        Ok(SkillSelection::Unchanged)
    );
    assert_eq!(
        skill_selection_from_params(&json!({"_meta": {}})),
        Ok(SkillSelection::Unchanged)
    );
    assert_eq!(
        skill_selection_from_params(&json!({})),
        Ok(SkillSelection::Unchanged)
    );
    // Malformed extension parameters are rejected, never silently ignored.
    let err = skill_selection_from_params(&json!({"_meta": {"jcode": {"skill": 7}}}))
        .expect_err("a non-string, non-null skill is malformed");
    assert!(err.contains("skill name or null"), "{err}");
}

#[test]
fn skill_selection_aliases_are_v1_compatibility_behind_the_canonical_field() {
    for params in [
        json!({"_meta": {"jcode": {"activeSkill": "$review"}}}),
        json!({"_meta": {"jcode": {"active_skill": "review"}}}),
        json!({"_meta": {"skill": "review"}}),
        json!({"_meta": {"activeSkill": "review"}}),
    ] {
        assert_eq!(
            skill_selection_from_params(&params),
            Ok(SkillSelection::Select("review".to_string())),
            "{params}"
        );
    }
    // Blank aliases keep the current selection, as they always have.
    assert_eq!(
        skill_selection_from_params(&json!({"_meta": {"skill": " "}})),
        Ok(SkillSelection::Unchanged)
    );
    // The canonical field wins over every alias, including its clear verb.
    assert_eq!(
        skill_selection_from_params(&json!({
            "_meta": {"jcode": {"skill": "review", "activeSkill": "interrogate"}}
        })),
        Ok(SkillSelection::Select("review".to_string()))
    );
    assert_eq!(
        skill_selection_from_params(&json!({
            "_meta": {"jcode": {"skill": null}, "skill": "interrogate"}
        })),
        Ok(SkillSelection::Clear)
    );
}

#[test]
fn a_clearing_prompt_clears_the_attachment_and_starts_no_turn() {
    let (_dir, skills) = project_skills(&["interrogate"]);

    // Empty prompt text: a selection-only action, answered without a turn, and
    // the attachment loses its skill.
    let plan = plan_prompt_skill(
        &SkillSelection::Clear,
        Some("interrogate".to_string()),
        &skills,
        String::new(),
        false,
    )
    .unwrap();
    assert!(
        plan.selection_only,
        "a bare clear starts no model turn: {plan:?}"
    );
    assert_eq!(
        plan.skill, None,
        "the prompt runs without the cleared skill"
    );
    assert!(plan.attach, "the clear rewrites the sticky selection");
    assert_eq!(
        prompt_response("end_turn", &TurnUsage::default()),
        json!({
            "stopReason": "end_turn",
            "_meta": { "jcode": { "turnId": null, "role": "none" } },
        })
    );

    // With text, the text is submitted unskilled exactly as written, even when
    // it looks like a skill invocation: only the clear decides the skill.
    let plan = plan_prompt_skill(
        &SkillSelection::Clear,
        Some("interrogate".to_string()),
        &skills,
        "/interrogate why".to_string(),
        false,
    )
    .unwrap();
    assert!(!plan.selection_only);
    assert_eq!(plan.skill, None);
    assert_eq!(plan.text, "/interrogate why");
}

#[test]
fn an_explicit_selection_wins_over_textual_recognition() {
    let (_dir, skills) = project_skills(&["interrogate", "review"]);

    // The metadata selection is the prompt's skill; the text stays a prompt.
    let plan = plan_prompt_skill(
        &SkillSelection::Select("review".to_string()),
        Some("interrogate".to_string()),
        &skills,
        "/interrogate why".to_string(),
        false,
    )
    .unwrap();
    assert_eq!(plan.skill, Some("review".to_string()));
    assert_eq!(plan.text, "/interrogate why");
    assert!(plan.attach);
    assert!(!plan.selection_only);

    // A selection-only selection still attaches and answers without a turn.
    let plan = plan_prompt_skill(
        &SkillSelection::Select("review".to_string()),
        None,
        &skills,
        String::new(),
        false,
    )
    .unwrap();
    assert!(plan.selection_only);
    assert_eq!(
        prompt_response("end_turn", &TurnUsage::default()),
        json!({
            "stopReason": "end_turn",
            "_meta": { "jcode": { "turnId": null, "role": "none" } },
        })
    );
    assert_eq!(plan.skill, Some("review".to_string()));

    // Textual invocations still select when no metadata is sent, and the
    // resulting sticky skill rides the next prompt unchanged.
    let plan = plan_prompt_skill(
        &SkillSelection::Unchanged,
        None,
        &skills,
        "$interrogate why".to_string(),
        false,
    )
    .unwrap();
    assert_eq!(plan.skill, Some("interrogate".to_string()));
    assert_eq!(plan.text, "why");
    let plan = plan_prompt_skill(
        &SkillSelection::Unchanged,
        Some("interrogate".to_string()),
        &skills,
        "next question".to_string(),
        false,
    )
    .unwrap();
    assert_eq!(plan.skill, Some("interrogate".to_string()));
    assert!(!plan.attach, "an unchanged selection is not rewritten");
    assert_eq!(plan.text, "next question");

    // Unknown explicit names are `-32602`, not a silently unskilled prompt.
    assert_eq!(
        plan_prompt_skill(
            &SkillSelection::Select("missing".to_string()),
            None,
            &skills,
            "go".to_string(),
            false,
        )
        .unwrap_err(),
        "Unknown skill: missing"
    );
}

#[test]
fn a_busy_session_rejects_an_explicit_skill_selection_before_insertion() {
    for selection in [
        SkillSelection::Select("interrogate".to_string()),
        SkillSelection::Clear,
    ] {
        let err = busy_skill_selection_error("s1", &selection, true)
            .unwrap_or_else(|| panic!("{selection:?} must be rejected while busy"));
        assert!(
            err.starts_with("Session s1 is already processing a prompt"),
            "{err}"
        );
        assert!(
            err.contains("skill selection needs an idle session"),
            "{err}"
        );
        assert_eq!(
            busy_skill_selection_error("s1", &selection, false),
            None,
            "an idle session honors {selection:?}"
        );
    }
    // A prompt that changes nothing is steered as before.
    assert_eq!(
        busy_skill_selection_error("s1", &SkillSelection::Unchanged, true),
        None
    );
}

#[test]
fn skill_commands_advertise_installed_skills_without_shadowing_builtins() {
    let (_dir, skills) = project_skills(&["interrogate", "model"]);
    let commands = acp_skill_commands(&skills);
    let names: Vec<&str> = commands
        .iter()
        .map(|command| command["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["interrogate"]);
    assert_eq!(commands[0]["description"], "Test interrogate");
}

#[test]
fn compatibility_methods_accept_host_field_names_and_aliases() {
    assert_eq!(
        compatibility_option_value(
            &json!({"modelId": "deepseek-v4-flash"}),
            &["modelId", "model"],
            "session/set_model"
        )
        .unwrap(),
        "deepseek-v4-flash"
    );
    assert_eq!(
        compatibility_option_value(
            &json!({"reasoningEffort": "high"}),
            &["effort", "reasoningEffort"],
            "session/set_reasoning_effort"
        )
        .unwrap(),
        "high"
    );
    assert!(
        compatibility_option_value(
            &json!({"effort": ""}),
            &["effort", "reasoningEffort"],
            "session/set_reasoning_effort"
        )
        .unwrap_err()
        .contains("non-empty")
    );
}

#[test]
fn cwd_must_be_absolute() {
    let params = json!({"cwd": "relative"});
    assert!(cwd_from_params(&params).is_err());
    let params = json!({"cwd": "/tmp"});
    assert_eq!(cwd_from_params(&params).unwrap(), Path::new("/tmp"));
}

#[test]
fn config_options_include_model_selector_and_effort_ladder() {
    let state = SessionUiState {
        provider_name: Some("openai".to_string()),
        model: Some("gpt-5.2".to_string()),
        available_models: vec!["gpt-5.2".to_string(), "gpt-5.2-codex".to_string()],
        reasoning_effort: Some("high".to_string()),
        available_reasoning_efforts: None,
    };
    let options = session_config_options(&state);
    assert_eq!(options.len(), 2);

    let model = &options[0];
    assert_eq!(model["id"], CONFIG_ID_MODEL);
    assert_eq!(model["category"], "model");
    assert_eq!(model["type"], "select");
    assert_eq!(model["currentValue"], "gpt-5.2");
    assert_eq!(model["options"].as_array().unwrap().len(), 2);

    let effort = &options[1];
    assert_eq!(effort["id"], CONFIG_ID_EFFORT);
    assert_eq!(effort["category"], "thought_level");
    assert_eq!(effort["currentValue"], "high");
    let effort_values: Vec<&str> = effort["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["value"].as_str().unwrap())
        .collect();
    assert!(effort_values.contains(&"medium"));
    assert!(
        !effort_values.iter().any(|value| value.starts_with("swarm")),
        "swarm sentinels are TUI-only and must not leak over ACP: {effort_values:?}"
    );
}

#[test]
fn config_options_current_model_prepended_when_not_listed() {
    let state = SessionUiState {
        provider_name: Some("anthropic".to_string()),
        model: Some("claude-opus-4-6".to_string()),
        available_models: vec!["claude-sonnet-4-5".to_string()],
        reasoning_effort: None,
        available_reasoning_efforts: None,
    };
    let options = session_config_options(&state);
    let model_values: Vec<&str> = options[0]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["value"].as_str().unwrap())
        .collect();
    assert_eq!(model_values[0], "claude-opus-4-6");
    assert!(model_values.contains(&"claude-sonnet-4-5"));
}

#[test]
fn legacy_models_catalog_is_emitted_alongside_config_options() {
    let state = SessionUiState {
        provider_name: Some("deepseek".to_string()),
        model: Some("deepseek-v4-flash".to_string()),
        available_models: vec!["deepseek-v4-pro".to_string()],
        reasoning_effort: Some("high".to_string()),
        available_reasoning_efforts: None,
    };
    let mut result = json!({"sessionId": "s1"});
    insert_session_configuration(&mut result, &state);

    assert!(result["configOptions"].is_array());
    assert_eq!(result["models"]["currentModelId"], "deepseek-v4-flash");
    let ids: Vec<&str> = result["models"]["availableModels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["modelId"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["deepseek-v4-flash", "deepseek-v4-pro"]);
}

#[test]
fn config_options_empty_without_model_state() {
    let options = session_config_options(&SessionUiState::default());
    assert!(options.is_empty());
}

#[test]
fn context_limit_falls_back_to_default_for_unknown_models() {
    let state = SessionUiState {
        provider_name: Some("mystery".to_string()),
        model: Some("mystery-model-9000".to_string()),
        available_models: Vec::new(),
        reasoning_effort: None,
        available_reasoning_efforts: None,
    };
    assert_eq!(
        state.context_limit(),
        crate::provider::DEFAULT_CONTEXT_LIMIT as u64
    );
}

// ACP shared sessions: peer gate, usage, close and message ids.

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
