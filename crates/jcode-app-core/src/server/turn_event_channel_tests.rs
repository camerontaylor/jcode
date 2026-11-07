#![cfg_attr(test, allow(clippy::await_holding_lock))]
//! Turn publication with several live attachments on one session: the shape a
//! TUI and a second client (another TUI, an ACP adapter) produce together.

use super::*;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use async_trait::async_trait;
use futures::stream;

struct IsolatedRuntimeDir {
    prev_runtime: Option<std::ffi::OsString>,
    _temp: tempfile::TempDir,
}

impl IsolatedRuntimeDir {
    fn new() -> Self {
        let temp = tempfile::TempDir::new().expect("runtime dir");
        let prev_runtime = std::env::var_os("JCODE_RUNTIME_DIR");
        crate::env::set_var("JCODE_RUNTIME_DIR", temp.path());
        crate::server::clear_reload_marker();
        Self {
            prev_runtime,
            _temp: temp,
        }
    }
}

impl Drop for IsolatedRuntimeDir {
    fn drop(&mut self) {
        match &self.prev_runtime {
            Some(prev) => crate::env::set_var("JCODE_RUNTIME_DIR", prev),
            None => crate::env::remove_var("JCODE_RUNTIME_DIR"),
        }
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

fn member(
    session_id: &str,
    event_txs: HashMap<String, mpsc::UnboundedSender<ServerEvent>>,
) -> SwarmMember {
    SwarmMember {
        session_id: session_id.to_string(),
        event_tx: event_txs.values().next().expect("one attachment").clone(),
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
    done_tx: mpsc::UnboundedSender<ProcessingCompletion>,
    _done_rx: mpsc::UnboundedReceiver<ProcessingCompletion>,
    sessions: SessionAgents,
}

impl TurnHarness {
    async fn new(session_id: &str, connections: usize, provider: Arc<dyn Provider>) -> Self {
        let registry = Registry::new(Arc::clone(&provider)).await;
        let mut session =
            crate::session::Session::create_with_id(session_id.to_string(), None, None);
        session.model = Some(provider.name().to_string());
        let agent = Arc::new(Mutex::new(Agent::new_with_session(
            provider, registry, session, None,
        )));
        let mut event_txs = HashMap::new();
        let mut senders = Vec::new();
        let mut receivers = Vec::new();
        for index in 0..connections {
            let (tx, rx) = mpsc::unbounded_channel::<ServerEvent>();
            event_txs.insert(format!("conn-{index}"), tx.clone());
            senders.push(tx);
            receivers.push(Some(rx));
        }
        let swarm_members = Arc::new(RwLock::new(HashMap::from([(
            session_id.to_string(),
            member(session_id, event_txs),
        )])));
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        let sessions = Arc::new(super::super::session_manager::SessionManager::from_agents(
            HashMap::from([(session_id.to_string(), Arc::clone(&agent))]),
        ));
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
            _done_rx: done_rx,
            sessions,
        }
    }

    fn take_receiver(&mut self, index: usize) -> mpsc::UnboundedReceiver<ServerEvent> {
        self.receivers[index].take().expect("receiver taken once")
    }

    fn swarm(&self) -> SwarmStatusRefs<'_> {
        SwarmStatusRefs {
            members: &self.swarm_members,
            swarms_by_id: &self.swarms_by_id,
            event_history: &self.event_history,
            event_counter: &self.event_counter,
            event_tx: &self.swarm_event_tx,
        }
    }

    /// Start a prompt on connection `index`, as the connection loop does for
    /// `Request::Message`.
    async fn start_prompt(
        &self,
        index: usize,
        id: u64,
        content: &str,
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
            },
            &self.session_id,
            state,
            &self.agent,
            &self.senders[index],
            &self.done_tx,
            Vec::new(),
            &self.swarm(),
        )
        .await;
    }
}

async fn next_event(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> ServerEvent {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("turn should produce events promptly")
        .expect("attachment channel should stay open")
}

/// Read until `predicate` matches, returning everything read.
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

fn scoped_payload(event: &ServerEvent) -> Option<(&crate::protocol::TurnIdentity, &ServerEvent)> {
    match event {
        ServerEvent::TurnEvent { identity, event } => Some((identity, event)),
        _ => None,
    }
}

fn is_scoped_done(event: &ServerEvent, request_id: u64) -> bool {
    scoped_payload(event)
        .is_some_and(|(_, event)| matches!(event, ServerEvent::Done { id } if *id == request_id))
}

/// Upstream's cancel path aborts a turn that ignores cancellation for 500 ms.
/// The aborted task never queues its own `done`, so the channel must publish
/// one: otherwise only the cancelling connection (via its direct reply) ever
/// learns that the turn ended, and every peer shows it running forever.
#[tokio::test]
async fn aborted_turn_publishes_a_scoped_terminal_to_every_attachment() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let mut harness = TurnHarness::new(
        "session_turn_channel_abort",
        2,
        Arc::new(HangingStreamProvider),
    )
    .await;
    let mut receivers = [harness.take_receiver(0), harness.take_receiver(1)];

    let mut client_is_processing = false;
    let mut message_id = None;
    let mut processing_session_id = None;
    let mut task = None;
    harness
        .start_prompt(
            0,
            21,
            "stream forever",
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut processing_session_id,
                task: &mut task,
            },
        )
        .await;

    let mut identities = Vec::new();
    for rx in &mut receivers {
        let events = read_until(rx, |event| {
            scoped_payload(event)
                .is_some_and(|(_, event)| matches!(event, ServerEvent::TextDelta { .. }))
        })
        .await;
        let Some(ServerEvent::TurnStarted { identity }) = events.first() else {
            panic!("the turn announces itself before it streams: {events:?}");
        };
        identities.push(identity.clone());
    }

    let task = task.take().expect("processing task handle");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    for (rx, identity) in receivers.iter_mut().zip(&identities) {
        let events = read_until(rx, |event| is_scoped_done(event, 21)).await;
        let (scope, _) = scoped_payload(events.last().unwrap()).unwrap();
        assert_eq!(
            scope, identity,
            "the terminal is scoped to the aborted turn"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ServerEvent::TurnStarted { .. })),
            "no other turn intervened: {events:?}"
        );
    }

    // The terminal is the last thing the aborted turn publishes, and it is
    // published before the runtime is released to the next turn.
    let next = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(lease) = harness.agent.lock().await.claim_server_turn() {
                break lease;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the drained turn releases the runtime");
    for rx in &mut receivers {
        assert!(rx.try_recv().is_err(), "exactly one terminal per turn");
    }
    drop(next);
}

/// A producer dropped without `finish` and without a terminal (aborted, or an
/// early return) still gets one, after everything it had already queued.
#[tokio::test]
async fn dropped_channel_publishes_its_terminal_after_queued_output() {
    let _lock = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(HangingStreamProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Agent::new(provider, registry);
    let (a_tx, mut a_rx) = mpsc::unbounded_channel();
    let (b_tx, mut b_rx) = mpsc::unbounded_channel();
    let members = Arc::new(RwLock::new(HashMap::from([(
        agent.session_id().to_string(),
        member(
            agent.session_id(),
            HashMap::from([("a".to_string(), a_tx), ("b".to_string(), b_tx)]),
        ),
    )])));
    let (fallback, mut fallback_rx) = mpsc::unbounded_channel();
    let lease = agent.claim_server_turn().unwrap();
    let identity = lease.identity().clone();
    let events = super::super::state::TurnEventChannel::new(
        agent.session_id().into(),
        Arc::clone(&members),
        fallback,
        &lease,
        33,
    );
    events
        .sender()
        .send(ServerEvent::TextDelta {
            text: "partial".into(),
        })
        .unwrap();
    drop(events);
    drop(lease);

    for rx in [&mut a_rx, &mut b_rx] {
        assert!(
            matches!(next_event(rx).await, ServerEvent::TurnStarted { identity: started } if started == identity)
        );
        assert!(matches!(
            scoped_payload(&next_event(rx).await),
            Some((scope, ServerEvent::TextDelta { text })) if scope == &identity && text == "partial"
        ));
        assert!(matches!(
            scoped_payload(&next_event(rx).await),
            Some((scope, ServerEvent::Done { id: 33 })) if scope == &identity
        ));
    }
    assert!(fallback_rx.try_recv().is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        while agent.claim_server_turn().is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("drain releases the runtime");
    assert!(a_rx.try_recv().is_err() && b_rx.try_recv().is_err());
}

/// Cancel announces `turn_stopped` to every attachment before it signals the
/// turn. Identity-aware readers drop unscoped turn events, so it must carry the
/// active turn's envelope (legacy transports unwrap it).
#[tokio::test]
async fn cancel_turn_stopped_is_scoped_to_the_active_turn_for_every_attachment() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let mut harness = TurnHarness::new(
        "session_turn_channel_cancel_scope",
        2,
        Arc::new(HangingStreamProvider),
    )
    .await;
    let mut receivers = [harness.take_receiver(0), harness.take_receiver(1)];

    let mut client_is_processing = false;
    let mut message_id = None;
    let mut processing_session_id = None;
    let mut task = None;
    harness
        .start_prompt(
            0,
            31,
            "cancel me",
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut processing_session_id,
                task: &mut task,
            },
        )
        .await;
    let mut identities = Vec::new();
    for rx in &mut receivers {
        let events = read_until(rx, |event| {
            scoped_payload(event)
                .is_some_and(|(_, event)| matches!(event, ServerEvent::TextDelta { .. }))
        })
        .await;
        let Some(ServerEvent::TurnStarted { identity, .. }) = events.first() else {
            panic!("the turn announces itself first: {events:?}");
        };
        identities.push(identity.clone());
    }

    let control = SessionControlHandle::cancel_only(
        harness.session_id.clone(),
        Arc::new(std::sync::Mutex::new(Vec::new())),
        InterruptSignal::new(),
    );
    let requester = harness.senders[0].clone();
    cancel_processing_message(
        &mut ProcessingState {
            client_is_processing: &mut client_is_processing,
            message_id: &mut message_id,
            session_id: &mut processing_session_id,
            task: &mut task,
        },
        &control,
        &requester,
        &harness.swarm(),
        Some(32),
        None,
        Some(&harness.sessions),
    )
    .await;

    for (rx, identity) in receivers.iter_mut().zip(&identities) {
        let events = read_until(rx, |event| is_scoped_done(event, 31)).await;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ServerEvent::TurnStopped { .. })),
            "turn_stopped must not be published unscoped: {events:?}"
        );
        assert!(
            events.iter().any(|event| matches!(
                scoped_payload(event),
                Some((scope, ServerEvent::TurnStopped {
                    reason: crate::protocol::TurnStopReason::Interrupted,
                    ..
                })) if scope == identity
            )),
            "every attachment sees the interrupt inside the turn's scope: {events:?}"
        );
    }
}

/// A turn-scoped cancel only reaches the turn it names. A stale identity (the
/// requester's turn already ended and a newer one started) must not stop the
/// newer turn; an unscoped cancel keeps the session-wide behaviour.
#[tokio::test]
async fn turn_scoped_cancel_only_targets_the_active_turn() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let mut harness = TurnHarness::new(
        "session_turn_channel_scoped_cancel",
        1,
        Arc::new(HangingStreamProvider),
    )
    .await;
    let mut rx = harness.take_receiver(0);

    let mut client_is_processing = false;
    let mut message_id = None;
    let mut processing_session_id = None;
    let mut task = None;
    harness
        .start_prompt(
            0,
            41,
            "keep running",
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut processing_session_id,
                task: &mut task,
            },
        )
        .await;
    let events = read_until(&mut rx, |event| {
        scoped_payload(event)
            .is_some_and(|(_, event)| matches!(event, ServerEvent::TextDelta { .. }))
    })
    .await;
    let Some(ServerEvent::TurnStarted {
        identity: active, ..
    }) = events.first().cloned()
    else {
        panic!("the turn announces itself first: {events:?}");
    };
    assert!(active.turn_id > 0);

    let older = crate::protocol::TurnIdentity {
        runtime_generation: active.runtime_generation.clone(),
        turn_id: active.turn_id - 1,
    };
    let other_runtime = crate::protocol::TurnIdentity {
        runtime_generation: "previous-runtime".into(),
        turn_id: active.turn_id,
    };
    let session_id = harness.session_id.clone();
    for stale in [&older, &other_runtime] {
        assert!(
            !cancel_targets_active_turn(&harness.sessions, &session_id, Some(stale)),
            "a stale turn identity must not cancel the running turn: {stale:?}"
        );
    }
    assert!(cancel_targets_active_turn(
        &harness.sessions,
        &session_id,
        Some(&active)
    ));
    assert!(cancel_targets_active_turn(
        &harness.sessions,
        &session_id,
        None
    ));
    assert!(
        !task
            .as_ref()
            .expect("turn still owns its task")
            .is_finished(),
        "the running turn is untouched"
    );

    task.take().unwrap().abort();
    let ended = read_until(&mut rx, |event| is_scoped_done(event, 41)).await;
    assert!(!ended.is_empty());
    tokio::time::timeout(Duration::from_secs(2), async {
        while harness.agent.lock().await.claim_server_turn().is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the aborted turn releases the runtime");
    assert!(
        !cancel_targets_active_turn(&harness.sessions, &session_id, Some(&active)),
        "once the turn ended, a cancel naming it is stale"
    );
}
