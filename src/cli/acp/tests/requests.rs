use super::*;

#[test]
fn plan_update_drops_cancelled_todos_and_defaults_required_fields() {
    let update = plan_update(
        &mut None,
        &todo_snapshot(json!([
            {"content": "cancelled work", "status": "cancelled", "priority": "high"},
            {"content": "unknown status", "status": "blocked", "priority": "urgent"},
            {"content": "no status", "priority": ""},
        ])),
    )
    .expect("plan update");
    // ACP has no cancelled state, so cancelled work is not part of the plan;
    // the required priority and status default instead of being guessed.
    assert_eq!(
        update["entries"],
        json!([
            {"content": "unknown status", "priority": "medium", "status": "pending"},
            {"content": "no status", "priority": "medium", "status": "pending"},
        ])
    );
}

#[test]
fn plan_update_reports_an_emptied_list_as_a_replacement() {
    let mut last = None;
    plan_update(
        &mut last,
        &todo_snapshot(json!([
            {"content": "one", "status": "pending", "priority": "high"},
        ])),
    )
    .expect("plan update");
    let cleared = plan_update(&mut last, &todo_snapshot(json!([]))).expect("cleared plan");
    assert_eq!(cleared["sessionUpdate"], "plan");
    assert_eq!(cleared["entries"], json!([]));
}

#[test]
fn plan_update_ignores_a_payload_that_is_not_a_todo_snapshot() {
    // Absence is legitimate: a malformed or unrelated payload yields an empty
    // plan, never entries invented from text.
    let update = plan_update(&mut None, "not json").expect("empty plan");
    assert_eq!(update["entries"], json!([]));
}

#[test]
fn todo_tool_names_are_the_daemons_canonical_name_and_aliases() {
    for name in ["todo", "todo_read", "todo_write", "todoread", "todowrite"] {
        assert!(is_todo_tool(name), "{name}");
    }
    for name in ["read", "bash", "todos", "todo_list"] {
        assert!(!is_todo_tool(name), "{name}");
    }
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
        last_plan: None,
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
        last_plan: None,
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
        last_plan: None,
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
        last_plan: None,
    };
    assert_eq!(
        state.context_limit(),
        crate::provider::DEFAULT_CONTEXT_LIMIT as u64
    );
}

// ACP shared sessions: peer gate, usage, close and message ids.
