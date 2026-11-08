use super::*;

/// The capability metadata a consumer gates every jcode behavior on, over the
/// real wire: `initialize` advertises it, and every successful attachment
/// reports its own instance id plus the effective `sharedControl` support.
///
/// No turn runs here on purpose: the metadata must be usable before a client
/// submits work.
#[tokio::test]
async fn acp_capability_metadata_identifies_each_attachment() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime_dir = short_runtime_dir(format!(
        "jcode-e2e-acp-capability-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir)?;
    let _runtime_env = EnvVarGuard::set("JCODE_RUNTIME_DIR", &runtime_dir);
    let socket_path = runtime_dir.join("jcode.sock");
    let debug_socket_path = runtime_dir.join("jcode-debug.sock");

    let provider: Arc<dyn Provider> = Arc::new(ScriptedTurnProvider {
        words: Arc::new(Mutex::new(VecDeque::new())),
        pause: Duration::from_millis(10),
    });
    let server_instance =
        server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone());
    let server_handle = tokio::spawn(async move { server_instance.run().await });

    let result = async {
        wait_for_socket(&socket_path).await?;
        let cwd = std::env::current_dir()?;
        let mut acp = AcpClient::spawn()?;

        // The default profile: `shared_control`, `peer_turns` and
        // `skill_selection` are honored, `raw_server_event` belongs to the
        // extended profiles only.
        let initialize = acp
            .request("initialize", serde_json::json!({ "protocolVersion": 1 }))
            .await?;
        let meta = &initialize["agentCapabilities"]["_meta"]["jcode"];
        assert_eq!(
            meta["interfaceVersion"], 1,
            "initialize advertises the jcode interface version: {initialize}"
        );
        assert_eq!(meta["profile"], "standard");
        assert_eq!(
            meta["extensions"],
            serde_json::json!([
                "shared_control",
                "peer_turns",
                "turn_correlation",
                "skill_selection"
            ])
        );
        assert_eq!(meta["sessionMcpServers"], false);
        assert_eq!(
            initialize["agentCapabilities"]["mcpCapabilities"],
            serde_json::json!({ "http": false, "sse": false })
        );

        let created = acp
            .request(
                "session/new",
                serde_json::json!({ "cwd": cwd, "mcpServers": [] }),
            )
            .await?;
        let created_instance = attachment_instance(&created, "session/new")?;
        let session_id = created["sessionId"]
            .as_str()
            .context("session/new returns a sessionId")?
            .to_string();

        // Loading/resuming an already-live session retains its daemon connection.
        // The identity follows that attachment rather than each RPC invocation.
        let loaded = acp
            .request(
                "session/load",
                serde_json::json!({ "sessionId": session_id, "cwd": cwd }),
            )
            .await?;
        let loaded_instance = attachment_instance(&loaded, "session/load")?;

        let resumed = acp
            .request(
                "session/resume",
                serde_json::json!({ "sessionId": session_id, "cwd": cwd }),
            )
            .await?;
        let resumed_instance = attachment_instance(&resumed, "session/resume")?;

        assert_eq!(created_instance, loaded_instance);
        assert_eq!(created_instance, resumed_instance);

        let different = acp
            .request("session/new", serde_json::json!({"cwd":cwd}))
            .await?;
        assert_ne!(
            created_instance,
            attachment_instance(&different, "different session")?
        );

        let mut peer = AcpClient::spawn()?;
        peer.request("initialize", serde_json::json!({"protocolVersion":1}))
            .await?;
        let peer_loaded = peer
            .request(
                "session/load",
                serde_json::json!({"sessionId":session_id,"cwd":cwd}),
            )
            .await?;
        assert_ne!(
            created_instance,
            attachment_instance(&peer_loaded, "peer load")?
        );
        peer.shutdown().await;

        acp.shutdown().await;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);
    result
}

/// `result._meta.jcode` of an attachment response, asserting the contract's
/// exact shape: a non-empty instance id, and exactly the four boolean
/// `sharedControl` fields, including effective daemon-backed detach support.
fn attachment_instance(result: &serde_json::Value, method: &str) -> Result<String> {
    let jcode = &result["_meta"]["jcode"];
    let instance = jcode["clientInstanceId"]
        .as_str()
        .with_context(|| format!("{method} reports a string clientInstanceId: {result}"))?;
    assert!(
        !instance.is_empty(),
        "{method} reports a non-empty clientInstanceId"
    );

    let shared = jcode["sharedControl"]
        .as_object()
        .with_context(|| format!("{method} reports sharedControl: {result}"))?;
    let mut fields: Vec<&str> = shared.keys().map(String::as_str).collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        ["detach", "implicitSteering", "observe", "stopObservedTurn"],
        "{method} reports exactly the contract's sharedControl fields: {shared:?}"
    );
    for (name, value) in shared {
        assert!(
            value.is_boolean(),
            "{method} reports sharedControl.{name} as a boolean: {value}"
        );
    }
    assert_eq!(
        shared["detach"], true,
        "{method} advertises effective detach support"
    );
    Ok(instance.to_string())
}

/// A `session/update` `notice`: informational, not part of session history.
fn is_notice(notification: &serde_json::Value) -> bool {
    notification["method"] == "session/update"
        && notification["params"]["update"]["sessionUpdate"] == "notice"
}

/// Read messages for `window`, returning what arrived. Proves a notice is not
/// sent after a response, without waiting on a later request to flush it.
async fn drain_for(client: &mut AcpClient, window: Duration) -> Result<Vec<serde_json::Value>> {
    let mut read = Vec::new();
    let deadline = Instant::now() + window;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match tokio::time::timeout(left, client.read_message()).await {
            Ok(Ok(message)) => read.push(message),
            Ok(Err(err)) => return Err(err),
            Err(_) => break,
        }
    }
    Ok(read)
}

/// Write one project-local skill into `root`, the same shape the TUI and the
/// adapter resolve.
fn write_project_skill(root: &std::path::Path, name: &str, description: &str) -> Result<()> {
    let dir = root.join(".jcode/skills").join(name);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\nDo {name}.\n"),
    )?;
    Ok(())
}

/// Skill selection and clear over the real wire: `_meta.jcode.skill` selects
/// without a model turn, `{"skill": null}` clears without one, and a clearing
/// prompt with text runs unskilled.
#[tokio::test]
async fn acp_skill_clear_round_trip() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime_dir = short_runtime_dir(format!(
        "jcode-e2e-acp-skill-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir)?;
    let _runtime_env = EnvVarGuard::set("JCODE_RUNTIME_DIR", &runtime_dir);
    let socket_path = runtime_dir.join("jcode.sock");
    let debug_socket_path = runtime_dir.join("jcode-debug.sock");

    let project = tempfile::tempdir()?;
    write_project_skill(project.path(), "interrogate", "Test interrogate")?;

    let provider: Arc<dyn Provider> = Arc::new(ScriptedTurnProvider {
        words: Arc::new(Mutex::new(
            ["alpha", "bravo", "charlie"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        )),
        // Long enough to hold a turn open while the test sends a second prompt.
        pause: Duration::from_millis(700),
    });
    let server_instance =
        server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone());
    let server_handle = tokio::spawn(async move { server_instance.run().await });

    let result = async {
        wait_for_socket(&socket_path).await?;
        let mut acp = AcpClient::spawn()?;
        let initialize = acp
            .request("initialize", serde_json::json!({ "protocolVersion": 1 }))
            .await?;
        assert!(
            initialize["agentCapabilities"]["_meta"]["jcode"]["extensions"]
                .as_array()
                .is_some_and(|extensions| extensions
                    .iter()
                    .any(|extension| extension == "skill_selection")),
            "skill selection is advertised: {initialize}"
        );
        let created = acp
            .request(
                "session/new",
                serde_json::json!({ "cwd": project.path(), "mcpServers": [] }),
            )
            .await?;
        let session_id = created["sessionId"]
            .as_str()
            .context("session/new returns a sessionId")?
            .to_string();
        acp.take_notifications();

        // Selection-only: a bare canonical selection attaches the skill, says
        // so, and answers without a model turn.
        let selected = acp
            .request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": "" }],
                    "_meta": { "jcode": { "skill": "interrogate" } },
                }),
            )
            .await?;
        assert_eq!(
            selected,
            serde_json::json!({
                "stopReason": "end_turn",
                "_meta": { "jcode": { "turnId": null, "role": "none" } },
            })
        );
        let updates = acp.take_notifications();
        assert!(
            agent_texts(&updates)
                .iter()
                .any(|text| text == "Activated skill: interrogate - Test interrogate\n\n"),
            "the selection reports the attached skill: {updates:#?}"
        );
        assert!(
            turn_notifications(&updates).is_empty(),
            "a selection-only action starts no turn: {updates:#?}"
        );

        let bare = acp
            .request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": "/interrogate" }],
                }),
            )
            .await?;
        assert_eq!(
            bare,
            serde_json::json!({
                "stopReason": "end_turn",
                "_meta": { "jcode": { "turnId": null, "role": "none" } },
            })
        );
        assert!(turn_notifications(&acp.take_notifications()).is_empty());

        // Clear with empty text: a selection-only action that starts no turn.
        let cleared = acp
            .request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [],
                    "_meta": { "jcode": { "skill": null } },
                }),
            )
            .await?;
        assert_eq!(
            cleared,
            serde_json::json!({
                "stopReason": "end_turn",
                "_meta": { "jcode": { "turnId": null, "role": "none" } },
            })
        );
        let updates = acp.take_notifications();
        assert!(
            agent_texts(&updates).is_empty(),
            "a clear says nothing in the transcript: {updates:#?}"
        );
        assert!(
            turn_notifications(&updates).is_empty(),
            "a bare clear starts no model turn: {updates:#?}"
        );

        // Clear with text: the text runs as a normal, unskilled turn.
        let prompt = "Reply with exactly the word: alpha";
        let ran = acp
            .request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": prompt }],
                    "_meta": { "jcode": { "skill": null } },
                }),
            )
            .await?;
        assert_eq!(ran["stopReason"], "end_turn");
        let updates = acp.take_notifications();
        let texts = agent_texts(&updates);
        assert!(
            texts.iter().any(|text| text == "alpha"),
            "the clearing prompt runs a model turn: {texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.starts_with("Activated skill:")),
            "the clear attaches nothing: {texts:?}"
        );

        // A missing key leaves the (now cleared) selection alone: the next
        // prompt is an ordinary one with no activation notice either.
        let after = acp
            .request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": "Reply with exactly the word: bravo" }],
                    "_meta": { "jcode": {} },
                }),
            )
            .await?;
        assert_eq!(after["stopReason"], "end_turn");
        let updates = acp.take_notifications();
        assert!(
            !agent_texts(&updates)
                .iter()
                .any(|text| text.starts_with("Activated skill:")),
            "a missing skill key must not reselect the cleared skill: {updates:#?}"
        );

        // A skill change cannot ride a steer: while a turn runs, an explicit
        // selection (here the clear) is rejected before insertion, and the
        // running prompt is untouched.
        let running_id = acp
            .send_request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": "Reply with exactly the word: charlie" }],
                }),
            )
            .await?;
        acp.wait_for_notification(|notification| is_agent_text(notification, "charlie"))
            .await?;
        let busy_id = acp
            .send_request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": "a steer that must not land" }],
                    "_meta": { "jcode": { "skill": null } },
                }),
            )
            .await?;
        let rejected = acp.wait_for_response(busy_id).await?;
        assert_eq!(
            rejected["error"]["code"], -32000,
            "a busy session rejects the skill change the way it rejects a busy prompt: {rejected}"
        );
        let message = rejected["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("already processing a prompt")
                && message.contains("a skill selection needs an idle session"),
            "the rejection says why and what to do: {message}"
        );
        let running = acp.wait_for_response(running_id).await?;
        assert_eq!(
            running["result"]["stopReason"], "end_turn",
            "the rejected steer left the running turn alone: {running}"
        );
        assert!(
            !agent_texts(&acp.take_notifications())
                .iter()
                .any(|text| text.contains("must not land")),
            "the rejected text was never delivered"
        );

        acp.shutdown().await;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);
    result
}

/// Ignored client `mcpServers` are reported once, after the new/load/resume
/// response that accepted them, on the connection that sent them. The response
/// itself never changes; an empty list says nothing and a non-array is still
/// `-32602`.
#[tokio::test]
async fn acp_ignored_mcp_servers_notice_follows_the_response() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime_dir = short_runtime_dir(format!(
        "jcode-e2e-acp-mcp-notice-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir)?;
    let _runtime_env = EnvVarGuard::set("JCODE_RUNTIME_DIR", &runtime_dir);
    let socket_path = runtime_dir.join("jcode.sock");
    let debug_socket_path = runtime_dir.join("jcode-debug.sock");

    let provider: Arc<dyn Provider> = Arc::new(ScriptedTurnProvider {
        words: Arc::new(Mutex::new(VecDeque::new())),
        pause: Duration::from_millis(10),
    });
    let server_instance =
        server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone());
    let server_handle = tokio::spawn(async move { server_instance.run().await });

    let result = async {
        wait_for_socket(&socket_path).await?;
        let cwd = std::env::current_dir()?;
        let mut acp = AcpClient::spawn()?;
        // A client that advertised the v1 `session.notices` capability: only
        // then may an agent send a `notice` session update.
        acp.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": 1,
                "clientCapabilities": { "session": { "notices": {} } },
            }),
        )
        .await?;

        // `session/new` with two client servers: the response comes first, then
        // exactly one notice naming both.
        let ignored = serde_json::json!([
            { "name": "fs", "command": "npx", "args": ["-y", "mcp-fs"] },
            { "name": "git", "command": "mcp-git" },
        ]);
        let new_id = acp
            .send_request(
                "session/new",
                serde_json::json!({ "cwd": cwd, "mcpServers": ignored }),
            )
            .await?;
        let mut messages = Vec::new();
        let mut response_at = None;
        let mut notice_at = None;
        for _ in 0..200 {
            let message = acp.read_message().await?;
            messages.push(message);
            let at = messages.len() - 1;
            if messages[at].get("method").is_none()
                && messages[at].get("id").and_then(|id| id.as_u64()) == Some(new_id)
            {
                response_at = Some(at);
            }
            if is_notice(&messages[at]) {
                notice_at = Some(at);
                break;
            }
        }
        let response_at = response_at.context("session/new answered")?;
        let notice_at = notice_at.context("the ignored servers were reported")?;
        assert!(
            response_at < notice_at,
            "the notice follows the response, never replaces it: {messages:#?}"
        );
        let response = &messages[response_at];
        assert!(
            response["error"].is_null() && response["result"].is_object(),
            "an ignored mcpServers array does not change the response: {response}"
        );
        let session_id = response["result"]["sessionId"]
            .as_str()
            .context("session/new returns a sessionId")?
            .to_string();
        let notice = &messages[notice_at]["params"];
        assert_eq!(notice["sessionId"], session_id.as_str());
        assert_eq!(notice["update"]["sessionUpdate"], "notice");
        assert_eq!(notice["update"]["severity"], "info");
        assert_eq!(
            notice["update"]["title"],
            "Client MCP servers not connected"
        );
        let description = notice["update"]["description"]
            .as_str()
            .context("the notice explains what happened")?;
        assert!(
            description.contains("fs, git"),
            "the notice names the ignored servers: {description}"
        );
        assert!(
            description.contains("daemon-side"),
            "the notice points at daemon-side MCP configuration: {description}"
        );
        // Exactly one notice: nothing further is sent for this attachment.
        let after = drain_for(&mut acp, Duration::from_millis(300)).await?;
        assert!(
            !after.iter().any(is_notice),
            "a nonempty array is reported once, not per server: {after:#?}"
        );

        // `session/load` and `session/resume` ignore an empty list silently.
        for method in ["session/load", "session/resume"] {
            let loaded = acp
                .request(
                    method,
                    serde_json::json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": [] }),
                )
                .await?;
            assert!(loaded.is_object(), "{method} succeeded: {loaded}");
            let after = drain_for(&mut acp, Duration::from_millis(300)).await?;
            let buffered = acp.take_notifications();
            assert!(
                !after.iter().any(is_notice) && !buffered.iter().any(is_notice),
                "an empty mcpServers array says nothing on {method}: {after:#?} {buffered:#?}"
            );
        }

        // A nonempty list on resume is reported the same way, after its
        // response.
        let resume_id = acp
            .send_request(
                "session/resume",
                serde_json::json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": ignored }),
            )
            .await?;
        let mut messages = Vec::new();
        let mut response_at = None;
        let mut notice_at = None;
        for _ in 0..200 {
            let message = acp.read_message().await?;
            messages.push(message);
            let at = messages.len() - 1;
            if messages[at].get("method").is_none()
                && messages[at].get("id").and_then(|id| id.as_u64()) == Some(resume_id)
            {
                response_at = Some(at);
            }
            if is_notice(&messages[at]) {
                notice_at = Some(at);
                break;
            }
        }
        assert!(
            response_at.context("session/resume answered")?
                < notice_at.context("session/resume reported the ignored servers")?,
            "resume reports the ignored servers after its response: {messages:#?}"
        );

        // A non-array shape is still a parameter error with no notice.
        let bad_id = acp
            .send_request(
                "session/new",
                serde_json::json!({ "cwd": cwd, "mcpServers": { "name": "fs" } }),
            )
            .await?;
        let bad = acp.wait_for_response(bad_id).await?;
        assert_eq!(
            bad["error"]["code"], -32602,
            "a non-array mcpServers stays invalid params: {bad}"
        );
        let after = drain_for(&mut acp, Duration::from_millis(300)).await?;
        let buffered = acp.take_notifications();
        assert!(
            !after.iter().any(is_notice) && !buffered.iter().any(is_notice),
            "a rejected shape reports no notice: {after:#?} {buffered:#?}"
        );

        // A client that did not advertise `session.notices` gets nothing at all
        // for the same nonempty array: v1 forbids the notice to that client, and
        // a message update is not a substitute for it.
        for client_capabilities in [
            None,
            Some(serde_json::json!({ "session": {} })),
            Some(serde_json::json!({ "session": { "notices": null } })),
        ] {
            let mut quiet = AcpClient::spawn()?;
            let mut params = serde_json::json!({ "protocolVersion": 1 });
            if let Some(capabilities) = client_capabilities.clone() {
                params["clientCapabilities"] = capabilities;
            }
            quiet.request("initialize", params.clone()).await?;
            let quiet_id = quiet
                .send_request(
                    "session/new",
                    serde_json::json!({ "cwd": cwd, "mcpServers": ignored }),
                )
                .await?;
            let quiet_response = quiet.wait_for_response(quiet_id).await?;
            assert!(
                quiet_response["result"]["sessionId"].is_string(),
                "an ignored array still creates the session: {quiet_response}"
            );
            let after = drain_for(&mut quiet, Duration::from_millis(300)).await?;
            let buffered = quiet.take_notifications();
            let updates: Vec<&serde_json::Value> = after.iter().chain(buffered.iter()).collect();
            assert!(
                !updates.iter().any(|message| is_notice(message)),
                "no notice without the client capability {params}: {updates:#?}"
            );
            assert!(
                !updates.iter().any(|message| {
                    message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                }),
                "no message update stands in for the notice {params}: {updates:#?}"
            );
            quiet.shutdown().await;
        }

        acp.shutdown().await;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);
    result
}
