//! End-to-end: an ACP client sharing a live daemon session with another client.
//!
//! Ports the raw two-client probe scenarios to a real `jcode acp` process:
//! the ACP adapter must mirror turns another client starts, and its own
//! `session/prompt` result must cover exactly its own turn.

use crate::test_support::*;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

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

/// Minimal ACP client over the real binary's stdin/stdout.
struct AcpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    /// Notifications received while waiting for a response.
    pending: Vec<serde_json::Value>,
}

impl AcpClient {
    fn spawn() -> Result<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_jcode"))
            .arg("acp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawn jcode acp")?;
        let stdin = child.stdin.take().context("acp stdin")?;
        let stdout = child.stdout.take().context("acp stdout")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
            pending: Vec::new(),
        })
    }

    async fn send(&mut self, value: serde_json::Value) -> Result<()> {
        let mut line = serde_json::to_string(&value)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read_message(&mut self) -> Result<serde_json::Value> {
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(30), self.stdout.read_line(&mut line))
            .await
            .context("timed out waiting for an ACP message")??;
        if read == 0 {
            anyhow::bail!("jcode acp exited before answering");
        }
        Ok(serde_json::from_str(line.trim())?)
    }

    /// Send a request and return its result, buffering notifications that
    /// arrive while waiting.
    async fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json_request(id, method, params)).await?;
        loop {
            let message = self.read_message().await?;
            if message.get("method").is_none()
                && message.get("id").and_then(|id| id.as_u64()) == Some(id)
            {
                if let Some(error) = message.get("error") {
                    anyhow::bail!("ACP {method} failed: {error}");
                }
                return Ok(message.get("result").cloned().unwrap_or_default());
            }
            self.pending.push(message);
        }
    }

    /// Wait for a notification matching `predicate`, reading more messages as
    /// needed. Non-matching notifications are buffered for later checks.
    async fn wait_for_notification(
        &mut self,
        predicate: impl Fn(&serde_json::Value) -> bool,
    ) -> Result<serde_json::Value> {
        if let Some(index) = self.pending.iter().position(&predicate) {
            return Ok(self.pending.remove(index));
        }
        for _ in 0..200 {
            let message = self.read_message().await?;
            if message.get("method").is_some() && predicate(&message) {
                return Ok(message);
            }
            self.pending.push(message);
        }
        anyhow::bail!("no matching ACP notification arrived")
    }

    /// Assert nothing arrives from the adapter for `window`.
    async fn expect_silence(&mut self, window: Duration) -> Result<()> {
        match tokio::time::timeout(window, self.read_message()).await {
            Err(_) => Ok(()),
            Ok(Ok(message)) => anyhow::bail!("adapter kept writing after session/close: {message}"),
            Ok(Err(err)) => Err(err),
        }
    }

    fn take_notifications(&mut self) -> Vec<serde_json::Value> {
        std::mem::take(&mut self.pending)
    }

    /// Read notifications in arrival order until `predicate` matches, returning
    /// everything read, so wire order can be asserted.
    async fn read_notifications_until(
        &mut self,
        predicate: impl Fn(&serde_json::Value) -> bool,
    ) -> Result<Vec<serde_json::Value>> {
        let mut read = Vec::new();
        for _ in 0..200 {
            let message = self.read_message().await?;
            if message.get("method").is_none() {
                anyhow::bail!("expected a notification, got {message}");
            }
            let done = predicate(&message);
            read.push(message);
            if done {
                return Ok(read);
            }
        }
        anyhow::bail!("no matching ACP notification arrived")
    }

    async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
    }
}

fn json_request(id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
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

    let words: VecDeque<String> = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedTurnProvider {
        words: Arc::new(Mutex::new(words)),
        pause: Duration::from_millis(400),
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
        let message_id = other.send_message("Reply with exactly the word: alpha").await?;
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

        let texts = agent_texts(&mirrored);
        assert!(
            texts.iter().any(|text| text == "alpha"),
            "the ACP client must receive the peer turn's content: {texts:?}"
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

        // The other client saw the ACP client's turn as a peer turn.
        let peer_events = collect_peer_turn(&mut other, "bravo").await?;
        assert!(
            peer_events
                .iter()
                .any(|event| matches!(event, ServerEvent::TurnStarted { .. })),
            "the daemon reports the ACP turn to the other client"
        );

        // --- Scenario 3: the other client prompts, ACP prompts mid-turn.
        let message_id = other
            .send_message("Count from 1 to 15, one number per line.")
            .await?;
        acp.wait_for_notification(|notification| {
            notification["method"] == "session/update"
                && notification["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                && notification["params"]["update"]["content"]["text"] == "charlie"
        })
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

        let queued = acp
            .request(
                "session/prompt",
                serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": "Reply with exactly the word: delta" }],
                }),
            )
            .await?;
        assert_eq!(queued["stopReason"], "end_turn");
        let queued_updates = acp.take_notifications();
        let queued_texts = agent_texts(&queued_updates);
        assert!(
            queued_texts.iter().any(|text| text == "delta"),
            "the queued prompt's own turn text arrives: {queued_texts:?}"
        );
        assert!(
            !queued_texts.iter().any(|text| text == "charlie"),
            "the turn that was running when we prompted must not leak into our result: {queued_texts:?}"
        );
        // The only turn brackets in this window belong to the peer turn that
        // was still running when we prompted; our own queued turn is bracketed
        // by its prompt response.
        for (state, turn_id) in turn_notifications(&queued_updates) {
            assert_eq!(
                turn_id, peer_turn_id,
                "only the peer turn is announced with _jcode/turn (state {state})"
            );
        }

        let other_events = collect_until_done_unix(&mut other, message_id).await?;
        assert!(
            other_events
                .iter()
                .any(|event| matches!(event, ServerEvent::Done { .. })),
            "the other client's turn completed"
        );

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

        // --- Scenario 4: closing the ACP view must not cancel a peer's turn,
        // and must stop reading the daemon for that session.
        let message_id = other
            .send_message("Reply with exactly the word: echo")
            .await?;
        acp.wait_for_notification(|notification| {
            notification["method"] == "session/update"
                && notification["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                && notification["params"]["update"]["content"]["text"] == "echo"
        })
        .await?;
        let closed = acp
            .request(
                "session/close",
                serde_json::json!({ "sessionId": session_id }),
            )
            .await?;
        assert!(closed.is_object(), "session/close answers: {closed}");

        // The other client's turn was untouched by the close.
        let echo_events = collect_until_done_unix(&mut other, message_id).await?;
        assert!(
            !echo_events.iter().any(|event| matches!(
                event,
                ServerEvent::TurnStopped {
                    reason: jcode::protocol::TurnStopReason::Interrupted,
                    ..
                }
            )),
            "closing the ACP view must not interrupt the peer's turn: {echo_events:?}"
        );
        assert!(
            echo_events
                .iter()
                .any(|event| matches!(event, ServerEvent::TextDelta { text } if text == "echo")),
            "the peer's turn completed normally"
        );

        // The connection is gone: a later peer turn produces no ACP output.
        let message_id = other
            .send_message("Reply with exactly the word: foxtrot")
            .await?;
        acp.expect_silence(Duration::from_millis(600)).await?;
        let foxtrot_events = collect_until_done_unix(&mut other, message_id).await?;
        assert!(
            foxtrot_events
                .iter()
                .any(|event| matches!(event, ServerEvent::Done { .. })),
            "the daemon still serves the session after the ACP view closed"
        );

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
