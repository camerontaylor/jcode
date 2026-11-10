//! End-to-end command scheduling: `/compact` and `/subagent` must do real work
//! through the daemon, not only parse in the adapter.
use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A provider that supports jcode compaction and records every completion, so
/// the test can witness the daemon summarizing actual prompt history and then
/// applying the summary to the next turn.
#[derive(Clone)]
struct CompactionProbeProvider {
    responses: Arc<Mutex<VecDeque<Vec<StreamEvent>>>>,
    calls: Arc<Mutex<Vec<Vec<Message>>>>,
    finished: Arc<AtomicUsize>,
}

impl CompactionProbeProvider {
    fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            calls: Arc::new(Mutex::new(Vec::new())),
            finished: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn queue(&self, events: Vec<StreamEvent>) {
        self.responses.lock().unwrap().push_back(events);
    }

    fn calls(&self) -> Vec<Vec<Message>> {
        self.calls.lock().unwrap().clone()
    }

    fn finished_calls(&self) -> usize {
        self.finished.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Provider for CompactionProbeProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.calls.lock().unwrap().push(messages.to_vec());
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        let items: Vec<Result<StreamEvent>> = events.into_iter().map(Ok).collect();
        let finished = Arc::clone(&self.finished);
        // Mark the call complete only once the consumer polls the stream to its
        // end, so the test does not treat "provider called" as "summary ready".
        let stream = stream::unfold(
            (items.into_iter(), finished),
            |(mut items, finished)| async move {
                match items.next() {
                    Some(item) => Some((item, (items, finished))),
                    None => {
                        finished.fetch_add(1, Ordering::SeqCst);
                        None
                    }
                }
            },
        );
        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "compaction-probe"
    }

    fn model(&self) -> String {
        "probe-model".to_string()
    }

    fn set_model(&self, _model: &str) -> Result<()> {
        Ok(())
    }

    fn available_models(&self) -> Vec<&'static str> {
        vec!["probe-model"]
    }

    fn supports_compaction(&self) -> bool {
        true
    }

    fn context_window(&self) -> usize {
        6_000
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[tokio::test]
async fn acp_conformance_compact_command_compacts_real_history() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let _socket_env = EnvVarGuard::set("JCODE_SOCKET", socket.clone());
    let provider = Arc::new(CompactionProbeProvider::new());
    for turn in 1..=6 {
        provider.queue(vec![
            StreamEvent::TextDelta(history_answer(turn)),
            StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            },
        ]);
    }
    provider.queue(vec![
        StreamEvent::TextDelta("compaction-summary-marker".to_string()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        },
    ]);
    provider.queue(vec![
        StreamEvent::TextDelta("after-compaction-answer".to_string()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        },
    ]);
    let daemon = server::Server::new_with_paths(
        provider.clone(),
        socket.clone(),
        runtime.path().join("debug.sock"),
    );
    let task = tokio::spawn(async move { daemon.run().await });
    let result = async {
        wait_for_socket(&socket).await?;
        let mut client = AcpClient::spawn()?;
        initialize(&mut client).await?;
        let cwd = std::env::current_dir()?;
        let created = client.request("session/new", json!({"cwd":cwd})).await?;
        let session = created["sessionId"]
            .as_str()
            .context("session/new returned no session id")?
            .to_string();

        for turn in 1..=6 {
            let question = format!("history-question-{turn}");
            let response = client
                .request(
                    "session/prompt",
                    json!({"sessionId":session,"prompt":[{"type":"text","text":question}]}),
                )
                .await?;
            assert_eq!(response["stopReason"], "end_turn");
            let expected = history_answer(turn);
            let updates = client.take_notifications();
            assert!(
                updates
                    .iter()
                    .any(|message| notification_text(message) == Some(expected.as_str())),
                "turn {turn} did not stream its answer: {updates:?}"
            );
        }
        assert_eq!(
            provider.calls().len(),
            6,
            "each prompt must reach the provider exactly once"
        );

        let response = client
            .request(
                "session/prompt",
                json!({"sessionId":session,"prompt":[{"type":"text","text":"/compact"}]}),
            )
            .await?;
        assert_eq!(response["stopReason"], "end_turn");
        timeout(
            Duration::from_secs(10),
            client.wait_for_notification(|message| {
                notification_text(message).is_some_and(|text| {
                    text.contains("Context Status") && text.contains("Compacting context")
                })
            }),
        )
        .await
        .context("manual compact status")??;
        wait_for_finished_calls(&provider, 7).await?;

        let calls = provider.calls();
        assert_eq!(
            calls.len(),
            7,
            "manual compaction must issue one provider summarization call"
        );
        let summarized = messages_text(&calls[6]);
        assert!(
            summarized.contains("history-question-1"),
            "compaction must read the oldest retained prompt: {summarized}"
        );
        assert!(
            summarized.contains(&history_answer(1)),
            "compaction must read the oldest retained answer: {summarized}"
        );
        assert!(
            !summarized.contains("history-question-6"),
            "compaction must leave the recent turns out of the summary: {summarized}"
        );

        let response = client
            .request(
                "session/prompt",
                json!({"sessionId":session,"prompt":[{"type":"text","text":"after-compaction-question"}]}),
            )
            .await?;
        assert_eq!(response["stopReason"], "end_turn");
        let updates = client.take_notifications();
        assert!(
            updates
                .iter()
                .any(|message| notification_text(message) == Some("after-compaction-answer")),
            "the turn after compaction did not complete: {updates:?}"
        );
        let calls = provider.calls();
        assert_eq!(
            calls.len(),
            8,
            "the post-compaction prompt must reach the provider once"
        );
        let after = messages_text(&calls[7]);
        assert!(
            after.contains("compaction-summary-marker"),
            "the applied summary must replace older history: {after}"
        );
        assert!(
            after.contains("history-question-6"),
            "recent turns must survive compaction: {after}"
        );
        assert!(
            !after.contains("history-question-1") && !after.contains(&history_answer(1)),
            "summarized turns must not remain in the next provider context: {after}"
        );
        client.shutdown().await;
        Ok(())
    }
    .await;
    task.abort();
    result
}

#[tokio::test]
async fn acp_conformance_subagent_command_spawns_live_worker() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let _socket_env = EnvVarGuard::set("JCODE_SOCKET", socket.clone());
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = Arc::new(MockProvider::gated(gate.clone()));
    provider.queue_response(vec![
        StreamEvent::TextDelta("child-part-1".to_string()),
        StreamEvent::TextDelta("child-part-2".to_string()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        },
    ]);
    let daemon = server::Server::new_with_paths(
        provider.clone(),
        socket.clone(),
        runtime.path().join("debug.sock"),
    );
    let task = tokio::spawn(async move { daemon.run().await });
    let result = async {
        wait_for_socket(&socket).await?;
        let mut parent = AcpClient::spawn()?;
        initialize(&mut parent).await?;
        let cwd = std::env::current_dir()?;
        let created = parent.request("session/new", json!({"cwd":cwd})).await?;
        let session = created["sessionId"]
            .as_str()
            .context("session/new returned no session id")?
            .to_string();

        let launch = parent
            .send_request(
                "session/prompt",
                json!({"sessionId":session,"prompt":[{"type":"text","text":"/subagent child-scout"}]}),
            )
            .await?;
        let tool_done = timeout(
            Duration::from_secs(10),
            parent.wait_for_notification(|message| terminal_tool_update(message).is_some()),
        )
        .await
        .context("subagent tool completion")??;
        let update = terminal_tool_update(&tool_done).context("tool update")?;
        assert_eq!(
            update["status"], "completed",
            "the /subagent swarm tool failed: {}",
            update["rawOutput"]
        );
        let output = update["rawOutput"]["output"]
            .as_str()
            .context("tool output")?;
        let child = spawned_child_id(output)
            .with_context(|| format!("unexpected /subagent output: {output}"))?
            .to_string();
        let response = parent.wait_for_response(launch).await?;
        assert_eq!(response["result"]["stopReason"], "end_turn");

        // The worker is still inside its first provider call: one delta is on
        // the wire and the response is parked at the mock gate. Attaching now
        // must observe the live turn rather than a completed transcript.
        let mut worker = AcpClient::spawn()?;
        initialize(&mut worker).await?;
        worker
            .request("session/resume", json!({"sessionId":child,"cwd":cwd}))
            .await?;
        let started = timeout(
            Duration::from_secs(10),
            worker.wait_for_notification(|message| {
                message["method"] == "_jcode/turn" && message["params"]["state"] == "started"
            }),
        )
        .await
        .context("worker turn attachment")??;
        assert_eq!(started["params"]["sessionId"], json!(child));
        let turn = started["params"]["turnId"].clone();
        let before_release = worker.take_notifications();
        assert!(
            !before_release
                .iter()
                .any(|message| notification_text(message) == Some("child-part-2")),
            "the gated worker streamed past its gate before release: {before_release:?}"
        );

        gate.add_permits(1);
        let live = timeout(
            Duration::from_secs(10),
            worker.wait_for_notification(|message| {
                notification_text(message) == Some("child-part-2")
            }),
        )
        .await
        .context("worker live delta")??;
        assert_eq!(live["params"]["sessionId"], json!(child));
        let finished = timeout(
            Duration::from_secs(10),
            worker.wait_for_notification(|message| {
                message["method"] == "_jcode/turn" && message["params"]["state"] != "started"
            }),
        )
        .await
        .context("worker turn terminal")??;
        assert_eq!(finished["params"]["turnId"], turn);
        assert_eq!(finished["params"]["state"], "completed");

        // The child's own live delta and terminal above are the request/outcome
        // proof. Do not constrain the global provider call count: a parent
        // title or follow-up completion is unrelated housekeeping.
        let listed = parent.request("session/list", json!({"cwd":cwd})).await?;
        assert!(
            listed["sessions"]
                .as_array()
                .is_some_and(|sessions| sessions.iter().any(|entry| entry["sessionId"] == json!(child))),
            "the spawned worker must be a registered session: {listed}"
        );

        parent.shutdown().await;
        worker.shutdown().await;
        Ok(())
    }
    .await;
    task.abort();
    result
}

fn history_answer(turn: usize) -> String {
    format!("history-answer-{turn}-{}", "x".repeat(600))
}

async fn wait_for_finished_calls(
    provider: &CompactionProbeProvider,
    expected: usize,
) -> Result<()> {
    timeout(Duration::from_secs(10), async {
        loop {
            if provider.finished_calls() >= expected {
                break Ok::<(), anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .with_context(|| {
        format!(
            "provider finished {} of {expected} expected completions",
            provider.finished_calls()
        )
    })??;
    Ok(())
}

fn messages_text(messages: &[Message]) -> String {
    messages
        .iter()
        .map(flatten_text_blocks)
        .collect::<Vec<_>>()
        .join("\n---\n")
}

fn notification_text(message: &Value) -> Option<&str> {
    message["params"]["update"]["content"]["text"].as_str()
}

fn terminal_tool_update(message: &Value) -> Option<&Value> {
    let update = &message["params"]["update"];
    if update["sessionUpdate"] != "tool_call_update" {
        return None;
    }
    match update["status"].as_str()? {
        "completed" | "failed" => Some(update),
        _ => None,
    }
}

fn spawned_child_id(output: &str) -> Option<&str> {
    output
        .strip_prefix("Spawned new agent: ")?
        .lines()
        .next()
        .map(str::trim)
        .filter(|id| !id.is_empty())
}
