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
    done_rx: mpsc::UnboundedReceiver<ProcessingCompletion>,
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
            done_rx,
            sessions,
        }
    }

    /// Settle finished turns the way the connection actor does: the
    /// completion queue holds each turn's publication reservation.
    fn drain_completions(&mut self) {
        while self.done_rx.try_recv().is_ok() {}
    }

    /// Wait until the runtime accepts a new turn.
    async fn wait_until_idle(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                self.drain_completions();
                if self.agent.lock().await.claim_server_turn().is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the finished turn releases the runtime");
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
                client_instance_id: Some(format!("conn-{index}")),
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
        let Some(ServerEvent::TurnStarted { identity, .. }) = events.first() else {
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
            matches!(
                &events[events.len() - 2],
                ServerEvent::TurnEnded {
                    identity: ended,
                    status: crate::protocol::TurnEndStatus::Cancelled,
                    ..
                } if ended == identity
            ),
            "an aborted turn ends cancelled, right before its terminal: {events:?}"
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
        crate::protocol::TurnOrigin::client(None, 33),
        None,
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
            matches!(next_event(rx).await, ServerEvent::TurnStarted { identity: started, .. } if started == identity)
        );
        assert!(matches!(
            scoped_payload(&next_event(rx).await),
            Some((scope, ServerEvent::TextDelta { text })) if scope == &identity && text == "partial"
        ));
        assert!(matches!(
            next_event(rx).await,
            ServerEvent::TurnEnded {
                identity: ended,
                status: crate::protocol::TurnEndStatus::Cancelled,
                ..
            } if ended == identity
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
        // The canceller also gets a direct interrupted/done reply, which can
        // race the channel's own end; read through both.
        let mut events =
            read_until(rx, |event| matches!(event, ServerEvent::TurnEnded { .. })).await;
        events.extend(read_until(rx, |event| is_scoped_done(event, 31)).await);
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
        let ended = events
            .iter()
            .position(|event| matches!(event, ServerEvent::TurnEnded { .. }))
            .expect("every attachment learns the turn ended");
        assert!(
            matches!(
                &events[ended],
                ServerEvent::TurnEnded {
                    identity: scope,
                    status: crate::protocol::TurnEndStatus::Cancelled,
                    ..
                } if scope == identity
            ),
            "a cancelled turn reports a cancelled end: {events:?}"
        );
        assert!(
            events[ended + 1..]
                .iter()
                .any(|event| is_scoped_done(event, 31)),
            "the channel's terminal follows turn_ended: {events:?}"
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

/// Streams `answer-<call>` for the Nth `complete` call, then waits for a gate
/// permit before ending, so a test decides when each turn finishes.
struct GatedNumberedProvider {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    gate: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl Provider for GatedNumberedProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let gate = Arc::clone(&self.gate);
        Ok(Box::pin(stream::unfold(0_u8, move |step| {
            let gate = Arc::clone(&gate);
            async move {
                match step {
                    0 => Some((Ok(StreamEvent::TextDelta(format!("answer-{call}"))), 1)),
                    1 => {
                        gate.acquire().await.expect("gate open").forget();
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
        "gated-numbered"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            calls: Arc::clone(&self.calls),
            gate: Arc::clone(&self.gate),
        })
    }
}

impl GatedNumberedProvider {
    fn new() -> (Arc<Self>, Arc<tokio::sync::Semaphore>) {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        (
            Arc::new(Self {
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                gate: Arc::clone(&gate),
            }),
            gate,
        )
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

/// The lifecycle-relevant shape of an attachment's stream.
fn lifecycle_summary(events: &[ServerEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::TurnStarted {
                origin,
                user_content,
                ..
            } => Some(format!(
                "started kind={:?} rid={:?} instance={:?} content={:?}",
                origin.as_ref().map(|origin| origin.kind),
                origin.as_ref().and_then(|origin| origin.request_id),
                origin
                    .as_ref()
                    .and_then(|origin| origin.client_instance_id.as_deref()),
                user_content
            )),
            ServerEvent::TurnEnded { status, .. } => Some(format!("ended {status:?}")),
            ServerEvent::TurnEvent { event, .. } => match &**event {
                ServerEvent::TextDelta { text } => Some(format!("delta {text}")),
                ServerEvent::Done { id } => Some(format!("done {id}")),
                ServerEvent::Error { id, .. } => Some(format!("error {id}")),
                _ => None,
            },
            ServerEvent::Done { id } => Some(format!("unscoped done {id}")),
            ServerEvent::Error { id, .. } => Some(format!("unscoped error {id}")),
            _ => None,
        })
        .collect()
}

fn started(index: usize, id: u64, content: &str) -> String {
    format!(
        "started kind=Some(Client) rid=Some({id}) instance=Some(\"conn-{index}\") content=Some({content:?})"
    )
}

/// Both attachments see the same ordered lifecycle around a prompt one of
/// them started, with the origin naming the starter. Attribution is published
/// before `turn_started` and withdrawn before `turn_ended`.
#[tokio::test]
async fn two_attachments_see_started_stream_ended_done_in_order() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let (provider, gate) = GatedNumberedProvider::new();
    let mut harness = TurnHarness::new("session_turn_channel_order", 2, provider).await;
    let mut receivers = [harness.take_receiver(0), harness.take_receiver(1)];
    let observer = harness
        .sessions
        .turn_observer(&harness.session_id)
        .expect("runtime observer");

    let mut client_is_processing = false;
    let mut message_id = None;
    let mut processing_session_id = None;
    let mut task = None;
    harness
        .start_prompt(
            0,
            77,
            "what is the answer",
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut processing_session_id,
                task: &mut task,
            },
        )
        .await;

    // Mid-turn, the attach snapshot names the running turn and its starter.
    let first = read_until(&mut receivers[0], |event| {
        matches!(event, ServerEvent::TurnStarted { .. })
    })
    .await;
    let Some(ServerEvent::TurnStarted { identity, .. }) = first.last().cloned() else {
        unreachable!()
    };
    let snapshot = observer.active_turn();
    assert_eq!(snapshot.active.as_ref(), Some(&identity));
    let attribution = snapshot.attribution.expect("running turn is attributed");
    assert_eq!(
        attribution.origin,
        crate::protocol::TurnOrigin::client(Some("conn-0".into()), 77)
    );
    assert_eq!(
        attribution.user_content.as_deref(),
        Some("what is the answer")
    );
    gate.add_permits(1);

    for (index, rx) in receivers.iter_mut().enumerate() {
        let mut events = if index == 0 {
            first.clone()
        } else {
            Vec::new()
        };
        loop {
            let event = next_event(rx).await;
            if matches!(event, ServerEvent::TurnEnded { .. }) {
                assert!(
                    observer.active_turn().active.is_none(),
                    "the turn is withdrawn from the attach snapshot before turn_ended"
                );
            }
            let done = is_scoped_done(&event, 77);
            events.push(event);
            if done {
                break;
            }
        }
        assert_eq!(
            lifecycle_summary(&events),
            vec![
                started(0, 77, "what is the answer"),
                "delta answer-1".to_string(),
                "ended Completed".to_string(),
                "done 77".to_string(),
            ],
            "attachment {index} sees started -> stream -> ended -> done"
        );
        assert!(events.iter().all(|event| match event {
            ServerEvent::TurnStarted { identity: id, .. }
            | ServerEvent::TurnEnded { identity: id, .. }
            | ServerEvent::TurnEvent { identity: id, .. } => id == &identity,
            _ => true,
        }));
    }
    assert!(observer.active_turn().attribution.is_none());
    task.take().unwrap().await.expect("processing task join");
}

/// A prompt sent while another turn runs never opens a nested turn. Turns run
/// one after the other, and each keeps the origin of the connection that sent
/// it.
#[tokio::test]
async fn overlapping_prompts_do_not_nest_and_keep_their_own_origin() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let (provider, gate) = GatedNumberedProvider::new();
    let mut harness = TurnHarness::new("session_turn_channel_overlap", 2, provider).await;
    let mut receivers = [harness.take_receiver(0), harness.take_receiver(1)];

    let (mut a_processing, mut a_message, mut a_session, mut a_task) = (false, None, None, None);
    let (mut b_processing, mut b_message, mut b_session, mut b_task) = (false, None, None, None);
    harness
        .start_prompt(
            0,
            11,
            "first prompt",
            &mut ProcessingState {
                client_is_processing: &mut a_processing,
                message_id: &mut a_message,
                session_id: &mut a_session,
                task: &mut a_task,
            },
        )
        .await;
    let mut streams: [Vec<ServerEvent>; 2] = [Vec::new(), Vec::new()];
    for (rx, stream) in receivers.iter_mut().zip(&mut streams) {
        stream.extend(
            read_until(rx, |event| {
                scoped_payload(event)
                    .is_some_and(|(_, event)| matches!(event, ServerEvent::TextDelta { .. }))
            })
            .await,
        );
    }

    // The second connection prompts while the first turn is running: it waits
    // for the runtime and is rejected while the first turn is still settling.
    {
        let mut state = ProcessingState {
            client_is_processing: &mut b_processing,
            message_id: &mut b_message,
            session_id: &mut b_session,
            task: &mut b_task,
        };
        tokio::join!(
            harness.start_prompt(1, 12, "second prompt", &mut state),
            async { gate.add_permits(1) },
        );
    }
    for (rx, stream) in receivers.iter_mut().zip(&mut streams) {
        stream.extend(read_until(rx, |event| is_scoped_done(event, 11)).await);
    }

    // Retry once the first turn has settled, as a client would after `error`.
    harness.wait_until_idle().await;
    if b_task.is_none() {
        harness
            .start_prompt(
                1,
                12,
                "second prompt",
                &mut ProcessingState {
                    client_is_processing: &mut b_processing,
                    message_id: &mut b_message,
                    session_id: &mut b_session,
                    task: &mut b_task,
                },
            )
            .await;
    }
    gate.add_permits(1);
    for (rx, stream) in receivers.iter_mut().zip(&mut streams) {
        stream.extend(read_until(rx, |event| is_scoped_done(event, 12)).await);
    }

    for (index, events) in streams.iter().enumerate() {
        let summary: Vec<String> = lifecycle_summary(events)
            .into_iter()
            .filter(|line| !line.starts_with("unscoped error 12"))
            .collect();
        assert_eq!(
            summary,
            vec![
                started(0, 11, "first prompt"),
                "delta answer-1".to_string(),
                "ended Completed".to_string(),
                "done 11".to_string(),
                started(1, 12, "second prompt"),
                "delta answer-2".to_string(),
                "ended Completed".to_string(),
                "done 12".to_string(),
            ],
            "attachment {index} sees two sequential, separately attributed turns"
        );
        let mut open: Option<&crate::protocol::TurnIdentity> = None;
        for event in events {
            match event {
                ServerEvent::TurnStarted { identity, .. } => {
                    assert!(open.is_none(), "turns must not nest: {events:?}");
                    open = Some(identity);
                }
                ServerEvent::TurnEnded { identity, .. } => {
                    assert_eq!(open.take(), Some(identity), "a turn ends its own scope");
                }
                ServerEvent::TurnEvent { identity, .. } => {
                    if let Some(open) = open {
                        assert_eq!(open, identity, "scoped output belongs to the open turn");
                    }
                }
                _ => {}
            }
        }
        assert!(open.is_none());
    }
    for task in [a_task.take(), b_task.take()].into_iter().flatten() {
        task.await.expect("processing task join");
    }
}

/// A panicking turn still ends: every attachment sees a failed `turn_ended`
/// immediately before the scoped error.
#[tokio::test]
async fn panicking_turn_reports_a_failed_end_to_every_attachment() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let mut harness =
        TurnHarness::new("session_turn_channel_panic", 2, Arc::new(PanickingProvider)).await;
    let mut receivers = [harness.take_receiver(0), harness.take_receiver(1)];

    let mut client_is_processing = false;
    let mut message_id = None;
    let mut processing_session_id = None;
    let mut task = None;
    harness
        .start_prompt(
            0,
            22,
            "panic please",
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut processing_session_id,
                task: &mut task,
            },
        )
        .await;

    for rx in &mut receivers {
        let events = read_until(rx, |event| {
            scoped_payload(event)
                .is_some_and(|(_, event)| matches!(event, ServerEvent::Error { id: 22, .. }))
        })
        .await;
        assert!(
            matches!(events.first(), Some(ServerEvent::TurnStarted { .. })),
            "the turn announces itself before it panics: {events:?}"
        );
        assert!(
            matches!(
                &events[events.len() - 2],
                ServerEvent::TurnEnded {
                    status: crate::protocol::TurnEndStatus::Failed,
                    message: Some(message),
                    ..
                } if message.contains("panicked")
            ),
            "a panicking turn reports a failed end before its error: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ServerEvent::TurnEnded { .. }))
                .count(),
            1
        );
    }
    task.take().unwrap().await.expect("processing task join");
}

/// The turn's attribution must be visible to an attaching client *before*
/// `turn_started` can be published: a client that registers its event sender
/// in that window builds its attach snapshot from the runtime's turn state.
#[tokio::test]
async fn turn_is_attributed_before_turn_started_is_announced() {
    let _lock = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(HangingStreamProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Agent::new(provider, registry);
    let observer = agent.server_turn_observer();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let members = Arc::new(RwLock::new(HashMap::from([(
        agent.session_id().to_string(),
        member(agent.session_id(), HashMap::from([("a".to_string(), tx)])),
    )])));
    // Block delivery: nothing can be announced while this is held.
    let blocked = members.write().await;
    let lease = agent.claim_server_turn().unwrap();
    let identity = lease.identity().clone();
    let origin = crate::protocol::TurnOrigin::client(Some("conn-a".into()), 41);
    let events = super::super::state::TurnEventChannel::new(
        agent.session_id().into(),
        Arc::clone(&members),
        mpsc::unbounded_channel().0,
        &lease,
        41,
        origin.clone(),
        Some("hi".into()),
    );
    tokio::task::yield_now().await;
    assert!(rx.try_recv().is_err(), "nothing announced yet");
    let snapshot = observer.active_turn();
    assert_eq!(snapshot.active.as_ref(), Some(&identity));
    assert_eq!(
        snapshot.attribution,
        Some(crate::agent::TurnAttribution {
            origin: origin.clone(),
            user_content: Some("hi".into()),
        }),
        "registered before announced"
    );
    drop(blocked);
    assert!(matches!(
        next_event(&mut rx).await,
        ServerEvent::TurnStarted { identity: started, origin: Some(announced), user_content: Some(content) }
            if started == identity && announced == origin && content == "hi"
    ));

    events.sender().send(ServerEvent::Done { id: 41 }).unwrap();
    assert!(matches!(
        next_event(&mut rx).await,
        ServerEvent::TurnEnded {
            status: crate::protocol::TurnEndStatus::Completed,
            ..
        }
    ));
    assert_eq!(
        observer.active_turn().attribution,
        None,
        "withdrawn before turn_ended"
    );
    assert!(is_scoped_done(&next_event(&mut rx).await, 41));
    events.finish().await;
    drop(lease);
}

/// Force replacement at the first await in cancellation dispatch. Holding the
/// fanout lock is a deterministic scheduling seam; no provider timing is used.
#[tokio::test]
async fn cancellation_dispatch_cannot_signal_a_successor() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let harness = TurnHarness::new("dispatch_replacement", 2, Arc::new(HangingStreamProvider)).await;
    for mode in 0..3 {
        let guard = harness.agent.lock().await;
        let lease = (mode < 2).then(|| guard.claim_server_turn().unwrap());
        let identity = lease.as_ref().map(|lease| lease.identity().clone());
        let signal = guard.graceful_shutdown_signal();
        drop(guard);
        let control = SessionControlHandle::cancel_only(
            harness.session_id.clone(), Arc::new(std::sync::Mutex::new(Vec::new())), signal.clone());
        let mut processing = false;
        let mut message = None;
        let mut session = None;
        let mut task = None;
        let mut state = ProcessingState { client_is_processing: &mut processing,
            message_id: &mut message, session_id: &mut session, task: &mut task };
        let swarm = harness.swarm();
        let blocked = harness.swarm_members.write().await;
        let cancel = super::client_processing::cancel_processing_message_for_turn(
            &mut state, &control, &harness.senders[0], &swarm, Some(2), None,
            Some(&harness.sessions), if mode == 0 { identity.as_ref() } else { None });
        tokio::pin!(cancel);
        assert!(futures::poll!(cancel.as_mut()).is_pending());
        drop(lease);
        let successor = harness.agent.lock().await.claim_server_turn().unwrap();
        let _registered = crate::turn_cancel_registry::register_active_turn(&harness.session_id, signal.clone());
        signal.reset(); // preparation of the successor clears only earlier cancellation
        drop(blocked);
        cancel.await;
        assert!(!signal.is_set(), "dispatch must not signal the successor (mode={mode})");
        drop(successor);
    }
}
