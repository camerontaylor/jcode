#![cfg_attr(test, allow(clippy::await_holding_lock))]
//! Daemon turn lifecycle tests: two connections on one session.
//!
//! These drive the real server turn path (`start_processing_message` →
//! `process_message_with_server_failover` → session fanout) with two live
//! attachments, which is exactly the multi-connection shape a TUI plus an ACP
//! client produce.

use super::*;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use async_trait::async_trait;
use futures::stream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

struct IsolatedRuntimeDir {
    _prev_runtime: Option<std::ffi::OsString>,
    _temp: tempfile::TempDir,
}

impl IsolatedRuntimeDir {
    fn new() -> Self {
        let temp = tempfile::TempDir::new().expect("runtime dir");
        let prev_runtime = std::env::var_os("JCODE_RUNTIME_DIR");
        crate::env::set_var("JCODE_RUNTIME_DIR", temp.path());
        crate::server::clear_reload_marker();
        Self {
            _prev_runtime: prev_runtime,
            _temp: temp,
        }
    }
}

impl Drop for IsolatedRuntimeDir {
    fn drop(&mut self) {
        match &self._prev_runtime {
            Some(prev) => crate::env::set_var("JCODE_RUNTIME_DIR", prev),
            None => crate::env::remove_var("JCODE_RUNTIME_DIR"),
        }
    }
}

/// Streams `answer-<call>` for the Nth `complete` call, so the two turns of an
/// overlapping-prompt test are distinguishable in the fanout stream.
struct NumberedStreamProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for NumberedStreamProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Box::pin(stream::unfold(0_u8, move |step| async move {
            match step {
                0 => Some((Ok(StreamEvent::TextDelta(format!("answer-{call}"))), 1)),
                1 => Some((
                    Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    }),
                    2,
                )),
                _ => None,
            }
        })))
    }

    fn name(&self) -> &str {
        "numbered-stream"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            calls: Arc::clone(&self.calls),
        })
    }
}

/// Streams one delta and then never finishes, so a turn can be aborted
/// mid-stream.
struct HangingStreamProvider;

#[async_trait]
impl Provider for HangingStreamProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(stream::unfold(0_u8, |step| async move {
            match step {
                0 => Some((
                    Ok(StreamEvent::TextDelta("streaming forever".to_string())),
                    1,
                )),
                _ => {
                    std::future::pending::<()>().await;
                    None
                }
            }
        })))
    }

    fn name(&self) -> &str {
        "hanging-stream"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

/// Panics while opening the provider stream, the way a provider bug unwinds
/// through a turn.
struct PanickingProvider;

#[async_trait]
impl Provider for PanickingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        panic!("provider exploded mid-turn")
    }

    fn name(&self) -> &str {
        "panicking"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

struct TurnHarness {
    session_id: String,
    agent: Arc<Mutex<Agent>>,
    swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    event_history: Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: broadcast::Sender<SwarmEvent>,
    senders: Vec<mpsc::UnboundedSender<ServerEvent>>,
    receivers: Vec<Option<mpsc::UnboundedReceiver<ServerEvent>>>,
    done_tx: mpsc::UnboundedSender<(u64, Result<()>, Option<String>)>,
    done_rx: mpsc::UnboundedReceiver<(u64, Result<()>, Option<String>)>,
}

impl TurnHarness {
    async fn new(session_id: &str, connection_ids: &[&str]) -> Self {
        let calls = Arc::new(AtomicUsize::new(0));
        Self::with_provider(
            session_id,
            connection_ids,
            Arc::new(NumberedStreamProvider { calls }),
        )
        .await
    }

    async fn with_provider(
        session_id: &str,
        connection_ids: &[&str],
        provider: Arc<dyn Provider>,
    ) -> Self {
        let registry = Registry::new(Arc::clone(&provider)).await;
        let mut session =
            crate::session::Session::create_with_id(session_id.to_string(), None, None);
        session.model = Some("numbered-stream".to_string());
        let agent = Arc::new(Mutex::new(Agent::new_with_session(
            provider, registry, session, None,
        )));

        let mut event_txs = HashMap::new();
        let mut senders = Vec::new();
        let mut receivers = Vec::new();
        for connection_id in connection_ids {
            let (tx, rx) = mpsc::unbounded_channel::<ServerEvent>();
            event_txs.insert(connection_id.to_string(), tx.clone());
            senders.push(tx);
            receivers.push(Some(rx));
        }
        let primary_tx = senders.first().expect("at least one connection").clone();
        let swarm_members = Arc::new(RwLock::new(HashMap::from([(
            session_id.to_string(),
            SwarmMember {
                session_id: session_id.to_string(),
                event_tx: primary_tx,
                event_txs,
                working_dir: None,
                swarm_id: None,
                swarm_enabled: false,
                status: "ready".to_string(),
                detail: None,
                task_label: None,
                friendly_name: None,
                report_back_to_session_id: None,
                latest_completion_report: None,
                role: "agent".to_string(),
                joined_at: Instant::now(),
                last_status_change: Instant::now(),
                is_headless: false,
                output_tail: None,
                todo_progress: None,
                todo_items: Vec::new(),
                runtime: crate::protocol::SwarmMemberRuntime::default(),
            },
        )])));

        let (done_tx, done_rx) = mpsc::unbounded_channel();
        Self {
            session_id: session_id.to_string(),
            agent,
            swarm_members,
            swarms_by_id: Arc::new(RwLock::new(HashMap::new())),
            event_history: Arc::new(RwLock::new(std::collections::VecDeque::new())),
            event_counter: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            swarm_event_tx: broadcast::channel(64).0,
            senders,
            receivers,
            done_tx,
            done_rx,
        }
    }

    fn take_receiver(&mut self, connection_index: usize) -> mpsc::UnboundedReceiver<ServerEvent> {
        self.receivers[connection_index]
            .take()
            .expect("connection receiver is taken once")
    }

    /// Start a prompt on the connection with `connection_index`, exactly as the
    /// connection loop does for `Request::Message`.
    async fn start_prompt(
        &self,
        connection_index: usize,
        id: u64,
        content: &str,
        client_instance_id: Option<String>,
        state: &mut ProcessingState<'_>,
    ) {
        start_processing_message(
            ProcessingMessage {
                id,
                content: content.to_string(),
                images: Vec::new(),
                system_reminder: None,
                active_skill: None,
                server_failover: false,
                client_instance_id,
            },
            &self.session_id,
            state,
            &self.agent,
            &self.senders[connection_index],
            &self.done_tx,
            Vec::new(),
            &SwarmStatusRefs {
                members: &self.swarm_members,
                swarms_by_id: &self.swarms_by_id,
                event_history: &self.event_history,
                event_counter: &self.event_counter,
                event_tx: &self.swarm_event_tx,
            },
        )
        .await;
    }
}

/// Read events until `predicate` matches, returning everything read.
async fn read_until(
    rx: &mut mpsc::UnboundedReceiver<ServerEvent>,
    predicate: impl Fn(&ServerEvent) -> bool,
) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    loop {
        let event = next_event(rx).await;
        let matched = predicate(&event);
        events.push(event);
        if matched {
            return events;
        }
    }
}

async fn next_event(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> ServerEvent {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("turn should produce events promptly")
        .expect("connection event channel should stay open")
}

/// Read events from `rx` until the terminal event for `request_id` arrives.
async fn collect_until_done(
    rx: &mut mpsc::UnboundedReceiver<ServerEvent>,
    request_id: u64,
) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    loop {
        let event = next_event(rx).await;
        let done = matches!(event, ServerEvent::Done { id } if id == request_id);
        events.push(event);
        if done {
            return events;
        }
    }
}

/// Read until every request id in `request_ids` has produced its terminal
/// `done`, returning the whole stream.
async fn collect_until_all_done(
    rx: &mut mpsc::UnboundedReceiver<ServerEvent>,
    request_ids: &[u64],
) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    loop {
        let event = next_event(rx).await;
        events.push(event);
        let remaining: Vec<u64> = request_ids
            .iter()
            .copied()
            .filter(|id| {
                !events
                    .iter()
                    .any(|event| matches!(event, ServerEvent::Done { id: done } if done == id))
            })
            .collect();
        if remaining.is_empty() {
            return events;
        }
    }
}

fn lifecycle_summary(events: &[ServerEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::TurnStarted {
                origin,
                user_content,
                ..
            } => Some(format!(
                "started rid={:?} instance={:?} content={:?}",
                origin.request_id, origin.client_instance_id, user_content
            )),
            ServerEvent::TurnEnded { status, .. } => Some(format!("ended {status:?}")),
            ServerEvent::TextDelta { text } => Some(format!("delta {text}")),
            ServerEvent::Done { id } => Some(format!("done {id}")),
            _ => None,
        })
        .collect()
}

/// Both attachments of one session must see the same ordered lifecycle around a
/// prompt one of them started.
#[tokio::test]
async fn two_connections_see_turn_started_stream_turn_ended_done_in_order() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let mut harness =
        TurnHarness::new("session_turn_lifecycle_fanout", &["conn-a", "conn-b"]).await;
    let mut origin_rx = harness.take_receiver(0);
    let mut peer_rx = harness.take_receiver(1);

    let mut client_is_processing = false;
    let mut message_id = None;
    let mut processing_session_id = None;
    let mut task = None;
    harness
        .start_prompt(
            0,
            77,
            "what is the answer",
            Some("conn-a".to_string()),
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut processing_session_id,
                task: &mut task,
            },
        )
        .await;

    let origin_events = collect_until_done(&mut origin_rx, 77).await;
    let peer_events = collect_until_done(&mut peer_rx, 77).await;

    for events in [&origin_events, &peer_events] {
        assert_eq!(
            lifecycle_summary(events),
            vec![
                "started rid=Some(77) instance=Some(\"conn-a\") content=Some(\"what is the answer\")"
                    .to_string(),
                "delta answer-1".to_string(),
                "ended Completed".to_string(),
                "done 77".to_string(),
            ],
            "both attachments must see started → stream → ended → done"
        );
        let turn_id = match events.first() {
            Some(ServerEvent::TurnStarted { turn_id, .. }) => turn_id.clone(),
            other => panic!("first event must be turn_started, got {other:?}"),
        };
        assert!(
            matches!(events.last(), Some(ServerEvent::Done { id: 77 })),
            "done must be terminal"
        );
        assert!(
            matches!(
                &events[events.len() - 2],
                ServerEvent::TurnEnded { turn_id: ended, .. } if ended == &turn_id
            ),
            "turn_ended must immediately precede the terminal done"
        );
    }

    // The running-turn snapshot the daemon reports to mid-turn attachers is
    // cleared once the turn ends.
    assert_eq!(
        crate::turn_lifecycle_registry::active_turn(&harness.session_id),
        None
    );

    if let Some(handle) = task.take() {
        handle.await.expect("processing task join");
    }
    let (done_id, result, _) = harness.done_rx.recv().await.expect("processing completion");
    assert_eq!(done_id, 77);
    result.expect("turn should succeed");
}

/// Two prompts on one session queue on the agent mutex. Their lifecycle pairs
/// must not nest, and each pair must name the connection that sent it.
#[tokio::test]
async fn overlapping_prompts_do_not_nest_and_keep_their_own_origin() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let mut harness =
        TurnHarness::new("session_turn_lifecycle_overlap", &["conn-a", "conn-b"]).await;
    let mut origin_rx = harness.take_receiver(0);
    let mut peer_rx = harness.take_receiver(1);

    let mut origin_processing = false;
    let mut origin_message_id = None;
    let mut origin_session_id = None;
    let mut origin_task = None;
    let mut peer_processing = false;
    let mut peer_message_id = None;
    let mut peer_session_id = None;
    let mut peer_task = None;

    harness
        .start_prompt(
            0,
            11,
            "first prompt",
            Some("conn-a".to_string()),
            &mut ProcessingState {
                client_is_processing: &mut origin_processing,
                message_id: &mut origin_message_id,
                session_id: &mut origin_session_id,
                task: &mut origin_task,
            },
        )
        .await;
    harness
        .start_prompt(
            1,
            12,
            "second prompt",
            Some("conn-b".to_string()),
            &mut ProcessingState {
                client_is_processing: &mut peer_processing,
                message_id: &mut peer_message_id,
                session_id: &mut peer_session_id,
                task: &mut peer_task,
            },
        )
        .await;

    let origin_events = collect_until_all_done(&mut origin_rx, &[11, 12]).await;
    let peer_events = collect_until_all_done(&mut peer_rx, &[11, 12]).await;

    for events in [&origin_events, &peer_events] {
        let mut open_turn: Option<(String, Option<u64>, Option<String>)> = None;
        let mut pairs = Vec::new();
        let mut ends = Vec::new();
        let mut starts = Vec::new();
        for event in events {
            match event {
                ServerEvent::TurnStarted {
                    turn_id, origin, ..
                } => {
                    assert!(
                        open_turn.is_none(),
                        "turn lifecycle pairs must not nest: {open_turn:?} was still open when {turn_id} started"
                    );
                    open_turn = Some((
                        turn_id.clone(),
                        origin.request_id,
                        origin.client_instance_id.clone(),
                    ));
                    starts.push(turn_id.clone());
                }
                ServerEvent::TurnEnded { turn_id, .. } => {
                    let (open_id, request_id, instance) = open_turn
                        .take()
                        .expect("turn_ended must close an open turn");
                    assert_eq!(&open_id, turn_id, "turn_ended must close its own turn");
                    pairs.push((request_id, instance));
                    ends.push(turn_id.clone());
                }
                _ => {}
            }
        }
        assert_eq!(open_turn, None, "every turn_started must be closed");
        assert_eq!(
            pairs,
            vec![
                (Some(11), Some("conn-a".to_string())),
                (Some(12), Some("conn-b".to_string())),
            ],
            "each lifecycle pair must name the connection that sent its prompt"
        );
        assert_eq!(starts, ends, "each turn must end with its own turn id");
        assert_ne!(starts[0], starts[1], "turns must have distinct ids");

        // Each turn's stream stays inside its own pair.
        let first_end = events
            .iter()
            .position(
                |event| matches!(event, ServerEvent::TurnEnded { turn_id, .. } if turn_id == &starts[0]),
            )
            .expect("first turn ended");
        let second_start = events
            .iter()
            .position(
                |event| matches!(event, ServerEvent::TurnStarted { turn_id, .. } if turn_id == &starts[1]),
            )
            .expect("second turn started");
        assert!(
            first_end < second_start,
            "the queued turn must start only after the running turn ended"
        );
        for (index, event) in events.iter().enumerate() {
            if let ServerEvent::TextDelta { text } = event {
                match text.as_str() {
                    "answer-1" => assert!(index < first_end, "first turn output before its end"),
                    "answer-2" => {
                        assert!(index > second_start, "second turn output after its start")
                    }
                    other => panic!("unexpected stream text {other}"),
                }
            }
        }
    }

    for handle in [origin_task.take(), peer_task.take()].into_iter().flatten() {
        handle.await.expect("processing task join");
    }
    assert_eq!(
        crate::turn_lifecycle_registry::active_turn(&harness.session_id),
        None
    );
}

/// A turn that is aborted mid-stream (the cancel grace period expired) must
/// still report its end to every attachment, or a peer-turn mirror keeps a
/// running turn forever.
#[tokio::test]
async fn aborted_turn_still_ends_for_every_attachment() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let mut harness = TurnHarness::with_provider(
        "session_turn_lifecycle_abort",
        &["conn-a", "conn-b"],
        Arc::new(HangingStreamProvider),
    )
    .await;
    let mut origin_rx = harness.take_receiver(0);
    let mut peer_rx = harness.take_receiver(1);

    let mut client_is_processing = false;
    let mut message_id = None;
    let mut processing_session_id = None;
    let mut task = None;
    harness
        .start_prompt(
            0,
            21,
            "stream forever",
            Some("conn-a".to_string()),
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut processing_session_id,
                task: &mut task,
            },
        )
        .await;

    // The turn is running and streaming on both connections before the abort.
    for rx in [&mut origin_rx, &mut peer_rx] {
        let events = read_until(rx, |event| matches!(event, ServerEvent::TextDelta { .. })).await;
        assert!(
            matches!(events.first(), Some(ServerEvent::TurnStarted { .. })),
            "the turn announces itself before it streams"
        );
    }

    task.take().expect("processing task handle").abort();

    for rx in [&mut origin_rx, &mut peer_rx] {
        let events = read_until(rx, |event| matches!(event, ServerEvent::TurnEnded { .. })).await;
        let ended = events
            .iter()
            .find(|event| matches!(event, ServerEvent::TurnEnded { .. }))
            .expect("turn ended");
        assert!(
            matches!(
                ended,
                ServerEvent::TurnEnded {
                    status: crate::protocol::TurnEndStatus::Cancelled,
                    ..
                }
            ),
            "an aborted turn reports a cancelled end: {ended:?}"
        );
    }
    assert_eq!(
        crate::turn_lifecycle_registry::active_turn(&harness.session_id),
        None,
        "an aborted turn must not stay published as the active turn"
    );
}

/// A panicking turn unwinds through the lifecycle pair: peers must still see a
/// (failed) end instead of a turn that never finishes.
#[tokio::test]
async fn panicking_turn_reports_a_failed_end_to_every_attachment() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let mut harness = TurnHarness::with_provider(
        "session_turn_lifecycle_panic",
        &["conn-a", "conn-b"],
        Arc::new(PanickingProvider),
    )
    .await;
    let mut origin_rx = harness.take_receiver(0);
    let mut peer_rx = harness.take_receiver(1);

    let mut client_is_processing = false;
    let mut message_id = None;
    let mut processing_session_id = None;
    let mut task = None;
    harness
        .start_prompt(
            0,
            22,
            "panic please",
            Some("conn-a".to_string()),
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut processing_session_id,
                task: &mut task,
            },
        )
        .await;

    for rx in [&mut origin_rx, &mut peer_rx] {
        let events = read_until(rx, |event| matches!(event, ServerEvent::Error { .. })).await;
        assert!(
            matches!(events.first(), Some(ServerEvent::TurnStarted { .. })),
            "the turn announces itself before it panics"
        );
        let ended_at = events
            .iter()
            .position(|event| matches!(event, ServerEvent::TurnEnded { .. }))
            .expect("a panicking turn still ends");
        let error_at = events
            .iter()
            .position(|event| matches!(event, ServerEvent::Error { .. }))
            .expect("the panic is reported");
        assert!(
            ended_at < error_at,
            "turn_ended precedes the terminal error: {events:?}"
        );
        assert!(
            matches!(
                &events[ended_at],
                ServerEvent::TurnEnded {
                    status: crate::protocol::TurnEndStatus::Failed,
                    ..
                }
            ),
            "a panicking turn reports a failed end: {:?}",
            events[ended_at]
        );
    }

    if let Some(handle) = task.take() {
        let _ = handle.await;
    }
    assert_eq!(
        crate::turn_lifecycle_registry::active_turn(&harness.session_id),
        None
    );
}
