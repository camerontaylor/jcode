//! End-to-end: an ACP client sharing a live daemon session with another client.
//!
//! Ports the raw two-client probe scenarios to a real `jcode acp` process:
//! the ACP adapter must mirror turns another client starts, and its own
//! `session/prompt` result must cover exactly its own turn.
//!
//! A prompt sent while a peer's turn runs is steered into that turn, like a
//! TUI or SDK follow-up: the daemon injects it at the turn's next safe point,
//! and the prompt adopts the turn, resolving with it.

use crate::test_support::acp::*;
use crate::test_support::*;
use std::collections::VecDeque;
use std::sync::Arc;

/// Streams a scripted word per turn, with a pause before the end so a second
/// client can prompt while the first turn is still running.
struct ScriptedTurnProvider {
    words: Arc<Mutex<VecDeque<String>>>,
    pause: Duration,
}

#[async_trait]
impl Provider for ScriptedTurnProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let word = self
            .words
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| "empty".to_string());
        let pause = self.pause;
        Ok(Box::pin(stream::unfold(0_u8, move |step| {
            let word = word.clone();
            async move {
                match step {
                    0 => Some((Ok(StreamEvent::TextDelta(word)), 1)),
                    1 => {
                        tokio::time::sleep(pause).await;
                        Some((
                            Ok(StreamEvent::MessageEnd {
                                stop_reason: Some("end_turn".to_string()),
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
        "scripted-turn"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            words: Arc::clone(&self.words),
            pause: self.pause,
        })
    }
}

/// `session/update` notifications carrying assistant text.
fn agent_texts(notifications: &[serde_json::Value]) -> Vec<String> {
    notifications
        .iter()
        .filter(|notification| notification["method"] == "session/update")
        .filter(|notification| {
            notification["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
        })
        .filter_map(|notification| {
            notification["params"]["update"]["content"]["text"]
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

/// `messageId` of every `session/update` that carries one, in order.
fn chunk_message_ids(notifications: &[serde_json::Value]) -> Vec<String> {
    notifications
        .iter()
        .filter(|notification| notification["method"] == "session/update")
        .filter_map(|notification| {
            notification["params"]["update"]["messageId"]
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

fn turn_notifications(notifications: &[serde_json::Value]) -> Vec<(String, String)> {
    notifications
        .iter()
        .filter(|notification| notification["method"] == "_jcode/turn")
        .map(|notification| {
            (
                notification["params"]["state"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                notification["params"]["turnId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// A `_jcode/turn` id is `"{runtime_generation}:{turn_id}"`.
fn split_turn_id(turn_id: &str) -> (String, u64) {
    let (generation, counter) = turn_id
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("turnId {turn_id} is generation:counter"));
    assert!(!generation.is_empty(), "turnId {turn_id} names its runtime");
    (
        generation.to_string(),
        counter
            .parse()
            .unwrap_or_else(|_| panic!("turnId {turn_id} ends with a turn counter")),
    )
}

fn is_turn_end(notification: &serde_json::Value) -> bool {
    notification["method"] == "_jcode/turn"
        && matches!(
            notification["params"]["state"].as_str(),
            Some("completed" | "failed" | "canceled")
        )
}

fn is_agent_text(notification: &serde_json::Value, text: &str) -> bool {
    notification["method"] == "session/update"
        && notification["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
        && notification["params"]["update"]["content"]["text"] == text
}

fn saw_interrupt(events: &[ServerEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            ServerEvent::TurnStopped {
                reason: jcode::protocol::TurnStopReason::Interrupted,
                ..
            }
        )
    })
}

#[tokio::test]
async fn acp_client_mirrors_peer_turns_and_keeps_its_own_prompt_separate() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime_dir = short_runtime_dir(format!(
        "jcode-e2e-acp-shared-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir)?;
    let _runtime_env = EnvVarGuard::set("JCODE_RUNTIME_DIR", &runtime_dir);
    let socket_path = runtime_dir.join("jcode.sock");
    let debug_socket_path = runtime_dir.join("jcode-debug.sock");

    // One word per provider call. A steer injected when a turn's answer ends
    // continues that turn with one more call: "kilo" and "lima".
    let words: VecDeque<String> = [
        "alpha", "bravo", "charlie", "kilo", "delta", "echo", "foxtrot", "lima", "golf", "hotel",
        "india",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedTurnProvider {
        words: Arc::new(Mutex::new(words)),
        // Long enough for a second client to attach mid-turn, short enough for
        // `test_support`'s one-second per-event read timeout.
        pause: Duration::from_millis(700),
    });
    let server_instance =
        server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone());
    let server_handle = tokio::spawn(async move { server_instance.run().await });

    let result = async {
        wait_for_socket(&socket_path).await?;
        let cwd = std::env::current_dir()?;

        // Client A: an ordinary daemon client, like a TUI window.
        let mut other = wait_for_subscribed_server_client(&socket_path).await?;
        let session_id = match other.get_history_event().await? {
            ServerEvent::History { session_id, .. } => session_id,
            other => anyhow::bail!("expected history, got {other:?}"),
        };

        // The ACP adapter attaches to A's live session.
        let mut acp = AcpClient::spawn()?;
        let initialize = acp
            .request("initialize", serde_json::json!({ "protocolVersion": 1 }))
            .await?;
        assert_eq!(initialize["protocolVersion"], 1);
        assert!(
            initialize["agentCapabilities"]["sessionCapabilities"]["list"].is_object(),
            "session/list must be advertised"
        );
        let loaded = acp
            .request(
                "session/load",
                serde_json::json!({ "sessionId": session_id, "cwd": cwd }),
            )
            .await?;
        assert!(
            loaded.is_object(),
            "session/load answers with the session configuration: {loaded}"
        );
        acp.take_notifications();

        // --- Scenario 1: the other client prompts, ACP mirrors the peer turn.
        let message_id = other
            .send_message("Reply with exactly the word: alpha")
            .await?;
        let other_events = collect_until_done_unix(&mut other, message_id).await?;
        assert!(
            other_events
                .iter()
                .any(|event| matches!(event, ServerEvent::TextDelta { text } if text == "alpha")),
            "the prompting client sees its own turn"
        );

        let mirrored = acp
            .read_notifications_until(|notification| {
                notification["method"] == "_jcode/turn"
                    && matches!(
                        notification["params"]["state"].as_str(),
                        Some("completed" | "failed" | "canceled")
                    )
            })
            .await?;
        let started = mirrored
            .iter()
            .find(|notification| {
                notification["method"] == "_jcode/turn"
                    && notification["params"]["state"] == "started"
            })
            .context("peer turn started notification")?;
        let ended = mirrored
            .iter()
            .find(|notification| {
                notification["method"] == "_jcode/turn"
                    && matches!(
                        notification["params"]["state"].as_str(),
                        Some("completed" | "failed" | "canceled")
                    )
            })
            .context("peer turn ended notification")?;
        assert_eq!(started["params"]["turnId"], ended["params"]["turnId"]);
        assert_eq!(started["params"]["origin"]["kind"], "client");
        assert_eq!(ended["params"]["state"], "completed");
        let first_turn_id = started["params"]["turnId"]
            .as_str()
            .context("turnId is a string")?
            .to_string();
        let (generation, first_counter) = split_turn_id(&first_turn_id);

        let texts = agent_texts(&mirrored);
        assert!(
            texts.iter().any(|text| text == "alpha"),
            "the ACP client must receive the peer turn's content: {texts:?}"
        );

        // Every chunk of the mirrored turn carries a stable message id, so the
        // client groups them into one message, and the peer's prompt is its own
        // message.
        let ids = chunk_message_ids(&mirrored);
        assert!(
            !ids.is_empty(),
            "mirrored chunks carry message ids: {mirrored:#?}"
        );
        let prompt_ids: Vec<&String> = ids.iter().filter(|id| id.ends_with(":user")).collect();
        assert_eq!(prompt_ids.len(), 1, "one id for the peer prompt: {ids:?}");
        assert_eq!(
            *prompt_ids[0],
            format!("{session_id}:{first_turn_id}:user"),
            "the peer prompt id is scoped to the turn id"
        );
        let stream_ids: Vec<&String> = ids.iter().filter(|id| !id.ends_with(":user")).collect();
        assert!(
            !stream_ids.is_empty(),
            "the peer turn's output carries an id: {ids:?}"
        );
        assert!(
            stream_ids.iter().all(|id| *id == stream_ids[0]),
            "all chunks of one assistant message share its id: {ids:?}"
        );
        assert!(
            stream_ids[0].starts_with(&format!("{session_id}:peer:{first_turn_id}:")),
            "peer output ids are scoped to the turn id: {ids:?}"
        );
        assert_ne!(
            stream_ids[0], prompt_ids[0],
            "the prompt and the answer are separate messages"
        );
        // Wire order: the bracket opens, then the prompt and the stream, then it
        // closes.
        let started_at = notification_position(&mirrored, started);
        let ended_at = notification_position(&mirrored, ended);
        let text_at = mirrored
            .iter()
            .position(|notification| {
                notification["method"] == "session/update"
                    && notification["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                    && notification["params"]["update"]["content"]["text"] == "alpha"
            })
            .context("peer turn text notification")?;
        let prompt_at = mirrored
            .iter()
            .position(|notification| {
                notification["method"] == "session/update"
                    && notification["params"]["update"]["sessionUpdate"] == "user_message_chunk"
            })
            .context("peer prompt notification")?;
        assert!(
            started_at < prompt_at && prompt_at < text_at && text_at < ended_at,
            "peer turn notifications must arrive in wire order: {mirrored:#?}"
        );

        // --- Scenario 2: the ACP client prompts; the result is its own turn.
        let result = acp
            .request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": "Reply with exactly the word: bravo" }],
                }),
            )
            .await?;
        assert_eq!(result["stopReason"], "end_turn");
        let own_updates = acp.take_notifications();
        let own_texts = agent_texts(&own_updates);
        assert!(
            own_texts.iter().any(|text| text == "bravo"),
            "the prompt's own turn text arrives as session/update: {own_texts:?}"
        );
        assert!(
            !own_texts.iter().any(|text| text == "alpha"),
            "a peer turn must not be reported as part of this prompt: {own_texts:?}"
        );
        assert!(
            turn_notifications(&own_updates).is_empty(),
            "own turns are bracketed by the prompt response, not _jcode/turn"
        );

        // The other client saw the ACP client's turn as a peer turn (a legacy
        // reader: the prompt arrives as `peer_user_message`).
        let peer_events = collect_peer_turn(&mut other, "bravo").await?;
        assert!(
            peer_events.iter().any(|event| matches!(
                event,
                ServerEvent::PeerUserMessage { content } if content.contains("bravo")
            )),
            "the daemon reports the ACP turn to the other client: {peer_events:?}"
        );

        // --- Scenario 3: the other client prompts, ACP prompts mid-turn. The
        // prompt is steered into the peer's turn: once injected, the turn is
        // the prompt's, and the prompt resolves when it ends.
        let message_id = other
            .send_message("Count from 1 to 15, one number per line.")
            .await?;
        acp.wait_for_notification(|notification| is_agent_text(notification, "charlie"))
            .await?;
        let peer_turn_id = acp
            .take_notifications()
            .iter()
            .find(|notification| {
                notification["method"] == "_jcode/turn"
                    && notification["params"]["state"] == "started"
            })
            .and_then(|notification| notification["params"]["turnId"].as_str())
            .map(str::to_string)
            .context("the running peer turn was announced")?;
        let (peer_generation, peer_counter) = split_turn_id(&peer_turn_id);
        assert_eq!(peer_generation, generation, "one runtime, one generation");
        assert!(
            peer_counter > first_counter,
            "turn ids increase: {first_turn_id} then {peer_turn_id}"
        );

        let steer_text = "Reply with exactly the word: kilo";
        let steer_id = acp
            .send_request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": steer_text }],
                }),
            )
            .await?;
        let steered = acp.wait_for_response(steer_id).await?;
        assert_eq!(
            steered["result"]["stopReason"], "end_turn",
            "a prompt during a peer turn joins it and ends with it: {steered}"
        );
        // Everything up to the answer, in wire order.
        let adopted = acp.take_notifications();
        let brackets = turn_notifications(&adopted);
        assert_eq!(
            brackets,
            vec![("completed".to_string(), peer_turn_id.clone())],
            "the adopted turn's bracket is closed once, and never reopened: {adopted:#?}"
        );
        let kilo = adopted
            .iter()
            .find(|notification| is_agent_text(notification, "kilo"))
            .with_context(|| format!("the turn's continuation answers the prompt: {adopted:#?}"))?;
        let kilo_id = kilo["params"]["update"]["messageId"]
            .as_str()
            .unwrap_or_default();
        assert!(
            kilo_id.starts_with(&format!("{session_id}:own:")),
            "after the injection the turn streams as the prompt's own output: {kilo_id}"
        );
        assert!(
            notification_position(&adopted, kilo) < adopted.iter().position(is_turn_end).unwrap(),
            "the bracket closes after the turn's output: {adopted:#?}"
        );
        assert!(
            !adopted.iter().any(|notification| {
                notification["params"]["update"]["sessionUpdate"] == "user_message_chunk"
                    && notification["params"]["update"]["content"]["text"] == steer_text
            }),
            "our own steer is not echoed as a peer message: {adopted:#?}"
        );
        let other_events = collect_until_done_unix(&mut other, message_id).await?;
        assert!(
            !saw_interrupt(&other_events),
            "the steer did not disturb the peer's turn: {other_events:?}"
        );
        assert!(
            other_events.iter().any(|event| matches!(
                event,
                ServerEvent::SoftInterruptInjected { content, .. } if content == steer_text
            )),
            "the peer sees the steer injected into its turn: {other_events:?}"
        );
        assert!(
            other_events
                .iter()
                .any(|event| matches!(event, ServerEvent::TextDelta { text } if text == "kilo")),
            "the peer sees the turn's continuation: {other_events:?}"
        );

        // Once the peer turn ended, the ACP prompt runs as its own turn.
        let own = acp
            .request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": "Reply with exactly the word: delta" }],
                }),
            )
            .await?;
        assert_eq!(own["stopReason"], "end_turn");
        let own_updates = acp.take_notifications();
        let own_texts = agent_texts(&own_updates);
        assert!(
            own_texts.iter().any(|text| text == "delta"),
            "the prompt's own turn text arrives: {own_texts:?}"
        );
        assert!(
            !own_texts.iter().any(|text| text == "charlie"),
            "the earlier peer turn must not leak into our result: {own_texts:?}"
        );
        assert!(
            turn_notifications(&own_updates).is_empty(),
            "our own turn is bracketed by its prompt response: {own_updates:#?}"
        );
        let _ = collect_peer_turn(&mut other, "delta").await?;

        // --- session/list reports the shared session, with liveness.
        let list = acp.request("session/list", serde_json::json!({})).await?;
        let sessions = list["sessions"].as_array().context("sessions array")?;
        let listed = sessions
            .iter()
            .find(|session| session["sessionId"] == session_id)
            .with_context(|| format!("session {session_id} missing from {list}"))?;
        assert_eq!(
            listed["_meta"]["jcode"]["live"], true,
            "a live session is reported as live: {listed}"
        );

        // --- Scenario 3b: a client attaching mid-turn replays history before it
        // mirrors any live peer output.
        let message_id = other
            .send_message("Reply with exactly the word: echo")
            .await?;
        acp.wait_for_notification(|notification| is_agent_text(notification, "echo"))
            .await?;

        let mut latecomer = AcpClient::spawn()?;
        latecomer
            .request("initialize", serde_json::json!({ "protocolVersion": 1 }))
            .await?;
        latecomer
            .request(
                "session/load",
                serde_json::json!({ "sessionId": session_id, "cwd": cwd }),
            )
            .await?;
        let attached = latecomer.take_notifications();
        let started_at = attached
            .iter()
            .position(|notification| {
                notification["method"] == "_jcode/turn"
                    && notification["params"]["state"] == "started"
            })
            .with_context(|| format!("the running turn is announced: {attached:#?}"))?;
        let replayed: Vec<&serde_json::Value> = attached[..started_at]
            .iter()
            .filter(|notification| notification["method"] == "session/update")
            .collect();
        assert!(
            !replayed.is_empty(),
            "history is replayed before live peer output: {attached:#?}"
        );
        let replay_ids = chunk_message_ids(&attached[..started_at]);
        assert_eq!(
            replay_ids.len(),
            replayed.len(),
            "every replayed message carries a message id: {attached:#?}"
        );
        let mut unique = replay_ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            replay_ids.len(),
            "consecutive same-role messages keep distinct ids: {replay_ids:?}"
        );
        assert!(
            replay_ids.iter().all(|id| id.contains(":msg:")),
            "the daemon reports each stored message's own index, so replay ids \
             survive a shift in the rendered list: {replay_ids:?}"
        );
        for notification in &attached[..started_at] {
            assert_ne!(
                notification["params"]["update"]["content"]["text"], "echo",
                "live peer output must not interleave with the replay: {attached:#?}"
            );
        }

        // The in-flight turn's prompt is already in the snapshot, so it must
        // not be announced a second time as a live user chunk: the attaching
        // client would show the same message twice.
        let in_flight_prompt = "Reply with exactly the word: echo";
        assert!(
            attached[..started_at].iter().any(|notification| {
                notification["method"] == "session/update"
                    && notification["params"]["update"]["sessionUpdate"] == "user_message_chunk"
                    && notification["params"]["update"]["content"]["text"] == in_flight_prompt
            }),
            "the snapshot already contains the in-flight prompt: {attached:#?}"
        );
        let duplicates: Vec<&serde_json::Value> = attached[started_at..]
            .iter()
            .filter(|notification| {
                notification["method"] == "session/update"
                    && notification["params"]["update"]["sessionUpdate"] == "user_message_chunk"
                    && notification["params"]["update"]["content"]["text"] == in_flight_prompt
            })
            .collect();
        assert!(
            duplicates.is_empty(),
            "the in-flight prompt must not be re-announced after the turn opens: {duplicates:#?}"
        );
        latecomer.shutdown().await;
        let _ = collect_until_done_unix(&mut other, message_id).await?;

        // --- Scenario 4: closing the ACP view must not cancel a peer's turn,
        // even one our prompt adopted, and must stop reading the daemon for
        // that session.
        let message_id = other
            .send_message("Reply with exactly the word: foxtrot")
            .await?;
        acp.wait_for_notification(|notification| is_agent_text(notification, "foxtrot"))
            .await?;
        // The newest bracket: earlier mirrored turns may still be buffered.
        let adopted_turn_id = acp
            .take_notifications()
            .iter()
            .rev()
            .find(|notification| {
                notification["method"] == "_jcode/turn"
                    && notification["params"]["state"] == "started"
            })
            .and_then(|notification| notification["params"]["turnId"].as_str())
            .map(str::to_string)
            .context("the running peer turn was announced")?;
        let steer_id = acp
            .send_request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": "Reply with exactly the word: lima" }],
                }),
            )
            .await?;
        // The steer was injected and the turn is ours now.
        acp.wait_for_notification(|notification| is_agent_text(notification, "lima"))
            .await?;
        let closed = acp
            .request(
                "session/close",
                serde_json::json!({ "sessionId": session_id }),
            )
            .await?;
        assert!(closed.is_object(), "session/close answers: {closed}");
        let released = acp.wait_for_response(steer_id).await?;
        assert_eq!(
            released["result"]["stopReason"], "cancelled",
            "closing the view releases the prompt that adopted the turn: {released}"
        );
        let ends: Vec<(String, String)> = turn_notifications(&acp.take_notifications())
            .into_iter()
            .filter(|(state, _)| state != "started")
            .collect();
        assert_eq!(
            ends,
            vec![("canceled".to_string(), adopted_turn_id)],
            "the adopted turn's bracket is closed for this view"
        );

        // The other client's turn was untouched by the close.
        let peer_events = collect_until_done_unix(&mut other, message_id).await?;
        assert!(
            !saw_interrupt(&peer_events),
            "closing the ACP view must not interrupt the peer's turn: {peer_events:?}"
        );
        assert!(
            peer_events
                .iter()
                .any(|event| matches!(event, ServerEvent::TextDelta { text } if text == "lima")),
            "the peer's turn ran to the end, steer included: {peer_events:?}"
        );

        // The connection is gone: a later peer turn produces no ACP output.
        let message_id = other
            .send_message("Reply with exactly the word: golf")
            .await?;
        acp.expect_silence(Duration::from_millis(600)).await?;
        let after_close = collect_until_done_unix(&mut other, message_id).await?;
        assert!(
            after_close
                .iter()
                .any(|event| matches!(event, ServerEvent::Done { .. })),
            "the daemon still serves the session after the ACP view closed"
        );

        // --- Scenario 5: session/cancel from the ACP view stops the turn it
        // shows running. A peer's turn (Paseo's stop during a TUI turn) is
        // cancelled for every client, and the mirror closes it as canceled.
        let mut stopper = AcpClient::spawn()?;
        stopper
            .request("initialize", serde_json::json!({ "protocolVersion": 1 }))
            .await?;
        stopper
            .request(
                "session/load",
                serde_json::json!({ "sessionId": session_id, "cwd": cwd }),
            )
            .await?;
        stopper.take_notifications();

        let message_id = other
            .send_message("Reply with exactly the word: hotel")
            .await?;
        stopper
            .wait_for_notification(|notification| is_agent_text(notification, "hotel"))
            .await?;
        let peer_turn_id = stopper
            .take_notifications()
            .iter()
            .find(|notification| {
                notification["method"] == "_jcode/turn"
                    && notification["params"]["state"] == "started"
            })
            .and_then(|notification| notification["params"]["turnId"].as_str())
            .map(str::to_string)
            .context("the running peer turn was announced")?;
        stopper
            .request(
                "session/cancel",
                serde_json::json!({ "sessionId": session_id }),
            )
            .await?;
        let ended = stopper.wait_for_notification(is_turn_end).await?;
        assert_eq!(ended["params"]["turnId"], peer_turn_id);
        assert_eq!(
            ended["params"]["state"], "canceled",
            "a stopped peer turn ends as canceled: {ended}"
        );
        let peer_events = collect_until_done_unix(&mut other, message_id).await?;
        assert!(
            saw_interrupt(&peer_events),
            "the peer's turn was interrupted by the ACP stop: {peer_events:?}"
        );

        // The same stop on our own running prompt answers it as cancelled. It
        // is sent at once: the daemon's stale stop of the peer turn must not
        // cancel it, and a steer the daemon queued while that turn was still
        // finishing is recovered.
        let prompt_id = stopper
            .send_request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": "Reply with exactly the word: india" }],
                }),
            )
            .await?;
        stopper
            .wait_for_notification(|notification| is_agent_text(notification, "india"))
            .await?;
        stopper
            .send_request(
                "session/cancel",
                serde_json::json!({ "sessionId": session_id }),
            )
            .await?;
        let cancelled = stopper.wait_for_response(prompt_id).await?;
        assert_eq!(
            cancelled["result"]["stopReason"], "cancelled",
            "a cancelled own prompt reports it: {cancelled}"
        );
        assert!(
            turn_notifications(&stopper.take_notifications()).is_empty(),
            "our own cancelled turn is not announced as a peer turn"
        );
        stopper.shutdown().await;

        acp.shutdown().await;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);
    result
}

/// Position of a notification inside a collected list, by identity of its
/// serialized form.
fn notification_position(notifications: &[serde_json::Value], needle: &serde_json::Value) -> usize {
    notifications
        .iter()
        .position(|notification| notification == needle)
        .unwrap_or_else(|| panic!("notification {needle} not in {notifications:#?}"))
}

/// Read events from a non-prompting client until it sees `text`, returning the
/// peer-turn lifecycle it observed.
async fn collect_peer_turn(client: &mut server::Client, text: &str) -> Result<Vec<ServerEvent>> {
    let mut events = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let event = timeout(Duration::from_secs(5), client.read_event()).await??;
        let saw_text = matches!(&event, ServerEvent::TextDelta { text: delta } if delta == text);
        events.push(event);
        if saw_text {
            return Ok(events);
        }
    }
    anyhow::bail!("timed out waiting for peer turn text {text}")
}

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

        // The default profile: `shared_control` and `peer_turns` are honored,
        // `raw_server_event` belongs to the extended profiles only.
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
            serde_json::json!(["shared_control", "peer_turns", "turn_correlation"])
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

        // Two more attachments to the same session, each a fresh daemon
        // connection of this adapter process.
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

        for (left, right) in [
            (&created_instance, &loaded_instance),
            (&loaded_instance, &resumed_instance),
            (&created_instance, &resumed_instance),
        ] {
            assert_ne!(
                left, right,
                "every attachment to one session reports its own instance id"
            );
        }

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
