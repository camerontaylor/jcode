//! Routing of daemon events on one ACP adapter connection.
//!
//! The adapter reads its daemon connection continuously (see
//! [`super::DaemonSession::spawn_reader`]) instead of only while one of its own
//! prompts is running. Every event is therefore routed here first:
//!
//! - replies to a request this adapter is waiting on go to that waiter;
//! - events between `turn_started` and `turn_ended` belong to that turn: this
//!   adapter's own turn feeds its `session/prompt`, a peer turn is mirrored to
//!   the ACP client;
//! - session-wide state (model, effort, title) is applied whenever it arrives;
//! - anything else is drained, so a peer's turn can never leak into the next
//!   prompt's stream (the misattribution bug this design exists to fix).
//!
//! The routing state is deliberately free of sockets and stdout so the whole
//! policy is unit-testable with scripted [`ServerEvent`] sequences.

use super::*;
use crate::protocol::{ActiveTurn, TurnEndStatus, TurnOrigin};

/// Decode one newline-delimited daemon event. `None` means the line is not an
/// event this build understands, which is expected across daemon/binary
/// version skew and must never end the session.
pub(super) fn decode_daemon_line(line: &str) -> Option<ServerEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    match serde_json::from_str::<ServerEvent>(trimmed) {
        Ok(event) => Some(event),
        Err(err) => {
            crate::logging::debug(&format!(
                "acp: skipping undecodable daemon line ({err}): {}",
                crate::util::truncate_str(trimmed, 200)
            ));
            None
        }
    }
}

/// What a pending request id is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingKind {
    /// An ACP `session/prompt` turn. Stream events for this adapter's own turn
    /// are delivered here.
    Prompt,
    /// A helper request/response pair (history, model catalog, config change).
    Reply,
}

pub(super) struct PendingRequest {
    kind: PendingKind,
    tx: mpsc::UnboundedSender<ServerEvent>,
}

/// A turn this adapter's client started.
#[derive(Debug, Clone)]
struct OwnTurn {
    turn_id: String,
    request_id: u64,
}

/// A turn another attachment started, being mirrored to the ACP client.
#[derive(Debug, Clone)]
struct PeerTurn {
    turn_id: String,
}

/// A peer turn whose terminal `done`/`error` is still outstanding.
#[derive(Debug, Clone)]
struct PeerTerminal {
    turn_id: String,
    request_id: u64,
}

/// A session-wide state change another client caused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionStateChange {
    Model {
        model: String,
        provider_name: Option<String>,
    },
    Effort {
        effort: Option<String>,
    },
    Renamed {
        title: String,
    },
}

/// What the adapter must render for a peer turn or a session-state change.
#[derive(Debug, Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "routing actions carry wire events unchanged; boxing them would only add allocation per streamed event"
)]
pub(super) enum RouteAction {
    PeerTurnStarted {
        turn_id: String,
        origin: TurnOrigin,
        user_content: Option<String>,
    },
    PeerTurnEvent(ServerEvent),
    PeerTurnEnded {
        turn_id: String,
        status: TurnEndStatus,
        message: Option<String>,
    },
    SessionState(SessionStateChange),
}

/// Routing state for one daemon connection.
pub(super) struct RouterState {
    instance_id: String,
    session_id: String,
    features: Vec<String>,
    live_sessions: Option<Vec<String>>,
    pending: HashMap<u64, PendingRequest>,
    own_turn: Option<OwnTurn>,
    peer_turn: Option<PeerTurn>,
    /// Peer turns whose terminal `done`/`error` has not arrived yet, keyed by
    /// the turn they belong to. A peer's terminal event carries the
    /// *originator's* connection-local request id, which can collide with one
    /// of ours, so the id alone is not enough: each entry expires when that
    /// turn's own terminal event is consumed, or when one of our turns
    /// completes (nothing older can still be outstanding in a useful way).
    peer_terminals: Vec<PeerTerminal>,
    /// Set once the connection is gone: no new waiter can be satisfied, and an
    /// open peer turn is closed.
    closed: bool,
}

impl RouterState {
    pub(super) fn new(instance_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            session_id: session_id.into(),
            features: Vec::new(),
            live_sessions: None,
            pending: HashMap::new(),
            own_turn: None,
            peer_turn: None,
            peer_terminals: Vec::new(),
            closed: false,
        }
    }

    pub(super) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Live session ids reported by the daemon, once any `History` arrived.
    pub(super) fn live_sessions(&self) -> Option<&[String]> {
        self.live_sessions.as_deref()
    }

    /// Whether this adapter's own turn is running right now. A prompt that is
    /// only queued behind a peer's turn is *not* running: nothing of ours holds
    /// the daemon's agent, so a session-wide cancel would hit the peer.
    pub(super) fn own_turn_running(&self) -> bool {
        self.own_turn.is_some()
    }

    pub(super) fn supports_turn_lifecycle(&self) -> bool {
        self.features
            .iter()
            .any(|feature| feature == crate::protocol::SERVER_FEATURE_TURN_LIFECYCLE)
    }

    pub(super) fn register(
        &mut self,
        request_id: u64,
        kind: PendingKind,
    ) -> mpsc::UnboundedReceiver<ServerEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        if self.closed {
            // The connection is gone; drop the sender so the caller fails
            // immediately instead of waiting for an event that cannot arrive.
            return rx;
        }
        self.pending.insert(request_id, PendingRequest { kind, tx });
        rx
    }

    pub(super) fn unregister(&mut self, request_id: u64) {
        self.pending.remove(&request_id);
    }

    /// Drop every waiter so tasks blocked on a channel learn the connection is
    /// gone, and close any peer turn still open. Returns the actions the
    /// adapter must still render (the peer turn's end).
    pub(super) fn close(&mut self) -> Vec<RouteAction> {
        self.closed = true;
        self.pending.clear();
        self.peer_terminals.clear();
        self.close_peer_turn(
            TurnEndStatus::Cancelled,
            Some("The connection closed before the turn ended.".to_string()),
        )
    }

    fn prompt_request_id(&self) -> Option<u64> {
        self.pending
            .iter()
            .find_map(|(id, pending)| (pending.kind == PendingKind::Prompt).then_some(*id))
    }

    fn is_own_origin(&self, origin: &TurnOrigin) -> bool {
        origin.kind == crate::protocol::TurnOriginKind::Client
            && origin.client_instance_id.as_deref() == Some(self.instance_id.as_str())
            && origin.request_id.is_some_and(|request_id| {
                self.pending
                    .get(&request_id)
                    .is_some_and(|pending| pending.kind == PendingKind::Prompt)
            })
    }

    /// Route one daemon event. Prompt and reply delivery happen through the
    /// registered channels; the returned actions are what the adapter must
    /// render for other clients' activity.
    pub(super) fn route(&mut self, event: ServerEvent) -> Vec<RouteAction> {
        let mut actions = Vec::new();

        // Session metadata first: a client attaching mid-turn needs the
        // running turn opened, and feature flags gate everything else.
        if let ServerEvent::History {
            session_id,
            server_features,
            active_turn,
            all_sessions,
            ..
        } = &event
        {
            self.session_id = session_id.clone();
            self.features = server_features.clone();
            self.live_sessions = Some(all_sessions.clone());
            if let Some(turn) = active_turn.clone() {
                actions.extend(self.open_peer_turn(turn));
            }
        }

        if let Some(request_id) = reply_request_id(&event) {
            let terminal = matches!(event, ServerEvent::Done { .. } | ServerEvent::Error { .. });
            if terminal {
                // A peer turn's completion carries the *originator's*
                // connection-local id, so it can collide with one of ours.
                // Consume the marker of the peer turn that owns this id; that
                // also closes a peer turn which never reported its own end.
                if let Some(index) = self
                    .peer_terminals
                    .iter()
                    .position(|terminal| terminal.request_id == request_id)
                {
                    let marker = self.peer_terminals.remove(index);
                    if self
                        .peer_turn
                        .as_ref()
                        .is_some_and(|peer| peer.turn_id == marker.turn_id)
                    {
                        let status = match &event {
                            ServerEvent::Error { .. } => TurnEndStatus::Failed,
                            _ => TurnEndStatus::Completed,
                        };
                        let message = match &event {
                            ServerEvent::Error { message, .. } => Some(message.clone()),
                            _ => None,
                        };
                        actions.extend(self.close_peer_turn(status, message));
                    }
                    return actions;
                }
            }
            if self.pending.contains_key(&request_id) {
                if terminal
                    && self
                        .own_turn
                        .as_ref()
                        .is_some_and(|turn| turn.request_id == request_id)
                {
                    self.own_turn = None;
                    // Peer terminals still outstanding once one of our turns has
                    // completed are stale: keeping them could swallow a later
                    // terminal event that reuses the id.
                    self.peer_terminals.clear();
                }
                self.deliver(request_id, event);
                return actions;
            }
            if terminal {
                crate::logging::debug(&format!(
                    "acp: dropping done/error for non-pending request id {request_id}"
                ));
                return actions;
            }
            // `ModelChanged` is also broadcast with id 0 and forwarded to peers
            // with the changer's id; anything else is a stale reply.
            if !matches!(event, ServerEvent::ModelChanged { .. }) {
                return actions;
            }
        }

        match event {
            ServerEvent::TurnStarted {
                turn_id,
                origin,
                user_content,
            } => {
                // Turn pairs never nest, so a peer turn still open here never
                // reported its end (an aborted or panicking peer). Close it
                // before opening anything else, or the new turn's stream would
                // be mirrored as that stale peer turn's output.
                actions.extend(self.close_peer_turn(
                    TurnEndStatus::Cancelled,
                    Some("The turn ended without a lifecycle end event.".to_string()),
                ));
                if self.is_own_origin(&origin) {
                    self.own_turn = Some(OwnTurn {
                        turn_id,
                        request_id: origin.request_id.unwrap_or_default(),
                    });
                } else {
                    actions.extend(self.open_peer_turn(ActiveTurn {
                        turn_id,
                        origin,
                        user_content,
                    }));
                }
            }
            ServerEvent::TurnEnded {
                turn_id,
                status,
                message,
            } => {
                if self
                    .own_turn
                    .as_ref()
                    .is_some_and(|turn| turn.turn_id == turn_id)
                {
                    // Keep `own_turn` until the terminal done/error: the
                    // interrupt/soft-interrupt notices that follow still belong
                    // to this prompt's stream.
                } else if self
                    .peer_turn
                    .as_ref()
                    .is_some_and(|turn| turn.turn_id == turn_id)
                {
                    self.peer_turn = None;
                    actions.push(RouteAction::PeerTurnEnded {
                        turn_id,
                        status,
                        message,
                    });
                }
            }
            // Superseded by `turn_started.user_content`.
            ServerEvent::PeerUserMessage { .. } => {}
            event => {
                if let Some(change) = session_state_change(&event) {
                    actions.push(RouteAction::SessionState(change));
                    return actions;
                }
                actions.extend(self.route_turn_event(event));
            }
        }

        actions
    }

    fn open_peer_turn(&mut self, turn: ActiveTurn) -> Vec<RouteAction> {
        // A `History` that reports our own running turn as active (a catalog
        // reply during our prompt) must not be mirrored as a peer turn.
        if self
            .own_turn
            .as_ref()
            .is_some_and(|own| own.turn_id == turn.turn_id)
        {
            return Vec::new();
        }
        if self
            .peer_turn
            .as_ref()
            .is_some_and(|peer| peer.turn_id == turn.turn_id)
        {
            return Vec::new();
        }
        if let Some(request_id) = turn.origin.request_id {
            self.peer_terminals.push(PeerTerminal {
                turn_id: turn.turn_id.clone(),
                request_id,
            });
        }
        self.peer_turn = Some(PeerTurn {
            turn_id: turn.turn_id.clone(),
        });
        vec![RouteAction::PeerTurnStarted {
            turn_id: turn.turn_id,
            origin: turn.origin,
            user_content: turn.user_content,
        }]
    }

    /// Close the peer turn that is currently open, if any.
    fn close_peer_turn(
        &mut self,
        status: TurnEndStatus,
        message: Option<String>,
    ) -> Vec<RouteAction> {
        let Some(peer) = self.peer_turn.take() else {
            return Vec::new();
        };
        vec![RouteAction::PeerTurnEnded {
            turn_id: peer.turn_id,
            status,
            message,
        }]
    }

    fn route_turn_event(&mut self, event: ServerEvent) -> Vec<RouteAction> {
        if self.peer_turn.is_some() {
            return vec![RouteAction::PeerTurnEvent(event)];
        }
        if let Some(own) = self.own_turn.as_ref() {
            let request_id = own.request_id;
            self.deliver(request_id, event);
            return Vec::new();
        }
        if !self.supports_turn_lifecycle() {
            // Legacy daemon: no turn boundaries, so keep today's behaviour for
            // our own prompt but drain anything that arrives while idle.
            if let Some(prompt_id) = self.prompt_request_id() {
                self.deliver(prompt_id, event);
            } else {
                crate::logging::debug("acp: draining idle daemon event");
            }
            return Vec::new();
        }
        crate::logging::debug("acp: dropping daemon event with no open turn");
        Vec::new()
    }

    fn deliver(&mut self, request_id: u64, event: ServerEvent) {
        let Some(pending) = self.pending.get(&request_id) else {
            crate::logging::debug(&format!(
                "acp: dropping event for unregistered request id {request_id}"
            ));
            return;
        };
        let _ = pending.tx.send(event);
    }
}

/// Request ids that answer a request this adapter may be waiting on.
fn reply_request_id(event: &ServerEvent) -> Option<u64> {
    match event {
        ServerEvent::Ack { id }
        | ServerEvent::Done { id }
        | ServerEvent::Error { id, .. }
        | ServerEvent::History { id, .. }
        | ServerEvent::ModelChanged { id, .. }
        | ServerEvent::ReasoningEffortChanged { id, .. }
        | ServerEvent::CompactResult { id, .. }
        | ServerEvent::ContextMessageAdded { id } => Some(*id),
        _ => None,
    }
}

fn session_state_change(event: &ServerEvent) -> Option<SessionStateChange> {
    match event {
        ServerEvent::ModelChanged {
            model,
            provider_name,
            error: None,
            ..
        } => Some(SessionStateChange::Model {
            model: model.clone(),
            provider_name: provider_name.clone(),
        }),
        ServerEvent::ReasoningEffortChanged {
            effort,
            error: None,
            ..
        } => Some(SessionStateChange::Effort {
            effort: effort.clone(),
        }),
        ServerEvent::SessionRenamed { display_title, .. } => Some(SessionStateChange::Renamed {
            title: display_title.clone(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OURS: &str = "acp-test";
    const PEER: &str = "acp-peer";

    fn router() -> RouterState {
        RouterState::new(OURS, "session-1")
    }

    /// A router that has seen a daemon `History` advertising the turn
    /// lifecycle feature.
    fn lifecycle_router() -> RouterState {
        let mut router = router();
        router.route(history_event(&["turn_lifecycle"], None));
        router
    }

    fn history_event(features: &[&str], active_turn: Option<Value>) -> ServerEvent {
        let mut value = json!({
            "type": "history",
            "id": 1,
            "session_id": "session-1",
            "messages": [],
            "server_features": features,
            "all_sessions": ["session-1"],
        });
        if let Some(active_turn) = active_turn {
            value["active_turn"] = active_turn;
        }
        serde_json::from_value(value).expect("history decodes")
    }

    fn turn_started(turn_id: &str, origin: TurnOrigin, content: Option<&str>) -> ServerEvent {
        ServerEvent::TurnStarted {
            turn_id: turn_id.to_string(),
            origin,
            user_content: content.map(str::to_string),
        }
    }

    fn turn_ended(turn_id: &str, status: TurnEndStatus) -> ServerEvent {
        ServerEvent::TurnEnded {
            turn_id: turn_id.to_string(),
            status,
            message: None,
        }
    }

    fn delta(text: &str) -> ServerEvent {
        ServerEvent::TextDelta {
            text: text.to_string(),
        }
    }

    fn ours(request_id: u64) -> TurnOrigin {
        TurnOrigin::client(Some(OURS.to_string()), request_id)
    }

    fn peer(request_id: u64) -> TurnOrigin {
        TurnOrigin::client(Some(PEER.to_string()), request_id)
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<ServerEvent> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn texts(events: &[ServerEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                ServerEvent::TextDelta { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn saw_done(events: &[ServerEvent], id: u64) -> bool {
        events
            .iter()
            .any(|event| matches!(event, ServerEvent::Done { id: done } if *done == id))
    }

    #[test]
    fn own_turn_streams_to_the_prompt_waiter_only() {
        let mut router = lifecycle_router();
        let mut prompt = router.register(77, PendingKind::Prompt);

        assert!(
            router
                .route(turn_started("t1", ours(77), Some("what is the answer")))
                .is_empty(),
            "an own turn is bracketed by the prompt response, not a peer mirror"
        );
        assert!(router.route(delta("answer")).is_empty());
        assert!(
            router
                .route(turn_ended("t1", TurnEndStatus::Completed))
                .is_empty()
        );
        assert!(router.route(ServerEvent::Done { id: 77 }).is_empty());

        let events = drain(&mut prompt);
        assert_eq!(texts(&events), vec!["answer".to_string()]);
        assert!(saw_done(&events, 77));

        // A stream event with no open turn is dropped, never queued for the
        // next prompt.
        assert!(router.route(delta("late")).is_empty());
        assert!(drain(&mut prompt).is_empty());
    }

    #[test]
    fn peer_turn_while_idle_is_mirrored_and_its_done_is_ignored() {
        let mut router = lifecycle_router();
        let mut prompt = router.register(77, PendingKind::Prompt);
        drain(&mut prompt);

        let actions = router.route(turn_started("t9", peer(5), Some("peer prompt")));
        assert!(matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnStarted { turn_id, user_content, .. }]
                if turn_id == "t9" && user_content.as_deref() == Some("peer prompt")
        ));

        let actions = router.route(delta("peer text"));
        assert!(matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnEvent(ServerEvent::TextDelta { text })] if text == "peer text"
        ));

        let actions = router.route(turn_ended("t9", TurnEndStatus::Completed));
        assert!(matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnEnded {
                status: TurnEndStatus::Completed,
                ..
            }]
        ));

        // The peer's terminal event carries its own connection-local id and
        // must not reach this adapter's pending prompt.
        assert!(router.route(ServerEvent::Done { id: 5 }).is_empty());
        assert!(drain(&mut prompt).is_empty());
    }

    #[test]
    fn peer_turn_queued_behind_an_own_turn_mirrors_without_touching_the_prompt() {
        let mut router = lifecycle_router();
        let mut prompt = router.register(77, PendingKind::Prompt);

        router.route(turn_started("t1", ours(77), Some("mine")));
        router.route(delta("own answer"));
        router.route(turn_ended("t1", TurnEndStatus::Completed));

        let actions = router.route(turn_started("t2", peer(8), Some("theirs")));
        assert!(matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnStarted { .. }]
        ));
        let actions = router.route(delta("peer answer"));
        assert!(matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnEvent(_)]
        ));
        let actions = router.route(turn_ended("t2", TurnEndStatus::Failed));
        assert!(matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnEnded {
                status: TurnEndStatus::Failed,
                ..
            }]
        ));

        // The own prompt's terminal still lands on its own channel.
        router.route(ServerEvent::Done { id: 77 });
        let events = drain(&mut prompt);
        assert_eq!(texts(&events), vec!["own answer".to_string()]);
        assert!(saw_done(&events, 77));
    }

    #[test]
    fn own_prompt_queued_behind_a_peer_turn_starts_only_on_its_own_turn_started() {
        let mut router = lifecycle_router();

        // A peer turn is already running when this adapter prompts.
        router.route(turn_started("t1", peer(4), Some("peer first")));
        let mut prompt = router.register(78, PendingKind::Prompt);
        assert!(
            router.route(delta("peer output")).len() == 1,
            "peer output keeps mirroring while our prompt is queued"
        );
        assert!(drain(&mut prompt).is_empty(), "queued prompt stays empty");

        // The daemon queues our message on the agent mutex, so our own turn
        // starts only after the peer's turn ended.
        router.route(turn_ended("t1", TurnEndStatus::Completed));
        assert!(router.route(ServerEvent::Done { id: 4 }).is_empty());
        assert!(
            router
                .route(turn_started("t2", ours(78), Some("mine")))
                .is_empty()
        );
        router.route(delta("own answer"));
        router.route(ServerEvent::Done { id: 78 });

        let events = drain(&mut prompt);
        assert_eq!(texts(&events), vec!["own answer".to_string()]);
        assert!(saw_done(&events, 78));
    }

    #[test]
    fn peer_done_with_a_colliding_id_does_not_complete_the_own_prompt() {
        let mut router = lifecycle_router();
        let mut prompt = router.register(77, PendingKind::Prompt);

        router.route(turn_started("t1", ours(77), Some("mine")));
        router.route(delta("own answer"));
        router.route(turn_ended("t1", TurnEndStatus::Completed));

        // A peer turn whose request id happens to be ours, and its completion.
        router.route(turn_started("t2", peer(77), Some("theirs")));
        router.route(delta("peer answer"));
        router.route(turn_ended("t2", TurnEndStatus::Completed));
        router.route(ServerEvent::Done { id: 77 });

        let events = drain(&mut prompt);
        assert!(
            !saw_done(&events, 77),
            "the peer's done must not finish our prompt: {events:?}"
        );
        assert_eq!(texts(&events), vec!["own answer".to_string()]);

        // Our own terminal event for the same id still arrives.
        router.route(ServerEvent::Done { id: 77 });
        assert!(saw_done(&drain(&mut prompt), 77));
    }

    #[test]
    fn undecodable_lines_are_skipped_and_never_end_the_session() {
        assert!(decode_daemon_line("").is_none());
        assert!(decode_daemon_line("not json at all").is_none());
        assert!(
            decode_daemon_line(r#"{"type":"event_from_a_newer_daemon"}"#).is_none(),
            "an unknown event type must be skipped, not fatal"
        );
        assert!(matches!(
            decode_daemon_line(r#"{"type":"text_delta","text":"hi"}"#),
            Some(ServerEvent::TextDelta { text }) if text == "hi"
        ));

        // A scripted line sequence with garbage in it still routes correctly.
        let mut router = lifecycle_router();
        let mut prompt = router.register(77, PendingKind::Prompt);
        for line in [
            r#"{"type":"turn_started","turn_id":"t1","origin":{"kind":"client","client_instance_id":"acp-test","request_id":77}}"#,
            "{ this is not json",
            r#"{"type":"text_delta","text":"answer"}"#,
            r#"{"type":"turn_ended","turn_id":"t1","status":"completed"}"#,
            r#"{"type":"done","id":77}"#,
        ] {
            if let Some(event) = decode_daemon_line(line) {
                router.route(event);
            }
        }
        let events = drain(&mut prompt);
        assert_eq!(texts(&events), vec!["answer".to_string()]);
        assert!(saw_done(&events, 77));
    }

    #[test]
    fn legacy_daemon_forwards_to_a_running_prompt_and_drains_idle_events() {
        // No `History` yet, so no feature flag: legacy behaviour.
        let mut router = router();
        assert!(
            router.route(delta("idle peer output")).is_empty(),
            "idle events are drained on a legacy daemon"
        );

        let mut prompt = router.register(77, PendingKind::Prompt);
        router.route(delta("running output"));
        router.route(ServerEvent::Done { id: 77 });
        let events = drain(&mut prompt);
        assert_eq!(texts(&events), vec!["running output".to_string()]);
        assert!(saw_done(&events, 77));
        // The adapter unregisters the prompt when its turn completes.
        router.unregister(77);

        // Back to idle: nothing accumulates for the next prompt.
        assert!(router.route(delta("after the turn")).is_empty());
        assert!(drain(&mut prompt).is_empty());
    }

    #[test]
    fn mid_turn_attach_opens_the_running_turn_from_history() {
        let mut router = router();
        let actions = router.route(history_event(
            &["turn_lifecycle"],
            Some(json!({
                "turn_id": "t5",
                "origin": {
                    "kind": "client",
                    "client_instance_id": PEER,
                    "request_id": 3
                },
                "user_content": "attached prompt",
            })),
        ));
        assert!(matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnStarted { turn_id, user_content, .. }]
                if turn_id == "t5" && user_content.as_deref() == Some("attached prompt")
        ));

        let actions = router.route(delta("mid-turn output"));
        assert!(matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnEvent(ServerEvent::TextDelta { text })] if text == "mid-turn output"
        ));

        let actions = router.route(turn_ended("t5", TurnEndStatus::Cancelled));
        assert!(matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnEnded {
                status: TurnEndStatus::Cancelled,
                ..
            }]
        ));
    }

    #[test]
    fn session_state_from_peers_becomes_an_update_and_replies_still_route() {
        let mut router = lifecycle_router();

        let actions = router.route(ServerEvent::ModelChanged {
            id: 0,
            model: "peer-model".to_string(),
            provider_name: Some("peer-provider".to_string()),
            context_window: None,
            error: None,
            resolved_credential: None,
            provider_oauth_email: None,
            provider_oauth_account_label: None,
            reasoning_effort: None,
        });
        assert!(matches!(
            actions.as_slice(),
            [RouteAction::SessionState(SessionStateChange::Model { model, .. })]
                if model == "peer-model"
        ));

        let actions = router.route(ServerEvent::SessionRenamed {
            session_id: "session-1".to_string(),
            title: Some("renamed".to_string()),
            display_title: "Renamed".to_string(),
        });
        assert!(matches!(
            actions.as_slice(),
            [RouteAction::SessionState(SessionStateChange::Renamed { title })] if title == "Renamed"
        ));

        // A model change this adapter asked for is a reply, not session state.
        let mut reply = router.register(90, PendingKind::Reply);
        assert!(
            router
                .route(ServerEvent::ModelChanged {
                    id: 90,
                    model: "mine".to_string(),
                    provider_name: None,
                    context_window: None,
                    error: None,
                    resolved_credential: None,
                    provider_oauth_email: None,
                    provider_oauth_account_label: None,
                    reasoning_effort: None,
                })
                .is_empty()
        );
        assert!(matches!(
            drain(&mut reply).as_slice(),
            [ServerEvent::ModelChanged { id: 90, .. }]
        ));
    }

    /// A peer turn that never reported its end (an aborted or panicking peer)
    /// must be closed before the next turn starts, or that turn's output is
    /// mirrored as the stale peer's.
    #[test]
    fn peer_turn_that_never_ended_is_closed_by_the_next_turn() {
        let mut router = lifecycle_router();
        let actions = router.route(turn_started("t1", peer(5), Some("peer prompt")));
        assert!(matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnStarted { .. }]
        ));
        assert!(matches!(
            router.route(delta("peer output")).as_slice(),
            [RouteAction::PeerTurnEvent(_)]
        ));

        // The peer turn is aborted: no turn_ended ever arrives. Our own prompt
        // is sent and its turn starts.
        let mut prompt = router.register(78, PendingKind::Prompt);
        let actions = router.route(turn_started("t2", ours(78), Some("mine")));
        assert!(
            matches!(
                actions.as_slice(),
                [RouteAction::PeerTurnEnded {
                    turn_id,
                    status: TurnEndStatus::Cancelled,
                    ..
                }] if turn_id == "t1"
            ),
            "the stale peer turn is closed when the next turn starts: {actions:?}"
        );

        // The own turn's output goes to the prompt, not the peer mirror.
        assert!(router.route(delta("own answer")).is_empty());
        router.route(ServerEvent::Done { id: 78 });
        let events = drain(&mut prompt);
        assert_eq!(texts(&events), vec!["own answer".to_string()]);
        assert!(saw_done(&events, 78));
    }

    /// The peer's terminal event closes a peer turn whose `turn_ended` never
    /// arrived, and is consumed so it cannot complete our prompt.
    #[test]
    fn peer_terminal_closes_a_peer_turn_that_never_ended() {
        let mut router = lifecycle_router();
        let mut prompt = router.register(77, PendingKind::Prompt);
        drain(&mut prompt);

        router.route(turn_started("t1", peer(9), Some("peer prompt")));
        let actions = router.route(ServerEvent::Done { id: 9 });
        assert!(
            matches!(
                actions.as_slice(),
                [RouteAction::PeerTurnEnded {
                    turn_id,
                    status: TurnEndStatus::Completed,
                    ..
                }] if turn_id == "t1"
            ),
            "the peer's completion closes its turn: {actions:?}"
        );
        assert!(drain(&mut prompt).is_empty());
    }

    /// Our own turn can start before a peer turn's terminal event arrives (the
    /// daemon reports the finished turn before it releases the agent lock). The
    /// marker must survive that start so the peer's late terminal event still
    /// cannot complete our prompt.
    #[test]
    fn peer_terminal_marker_survives_our_own_turn_start() {
        let mut router = lifecycle_router();

        // A peer turn runs and ends, but its terminal event is still in flight.
        router.route(turn_started("t1", peer(77), Some("peer prompt")));
        router.route(turn_ended("t1", TurnEndStatus::Completed));

        // Our own prompt was queued behind it, so its turn starts first.
        let mut prompt = router.register(77, PendingKind::Prompt);
        router.route(turn_started("t2", ours(77), Some("mine")));
        assert!(router.route(delta("own answer")).is_empty());

        // The peer's terminal event for the colliding id must be consumed.
        assert!(router.route(ServerEvent::Done { id: 77 }).is_empty());
        assert!(
            !saw_done(&drain(&mut prompt), 77),
            "the peer's late done must not complete our prompt"
        );

        // Our own terminal event still lands.
        router.route(ServerEvent::Done { id: 77 });
        assert!(saw_done(&drain(&mut prompt), 77));
    }

    /// Closing the connection fails its waiters, refuses new ones, and ends a
    /// peer turn that is still open.
    #[test]
    fn close_releases_waiters_and_ends_an_open_peer_turn() {
        let mut router = lifecycle_router();
        let mut waiter = router.register(70, PendingKind::Reply);
        router.route(turn_started("t1", peer(5), Some("peer prompt")));

        let actions = router.close();
        assert!(
            matches!(
                actions.as_slice(),
                [RouteAction::PeerTurnEnded {
                    turn_id,
                    status: TurnEndStatus::Cancelled,
                    ..
                }] if turn_id == "t1"
            ),
            "closing ends the open peer turn: {actions:?}"
        );
        assert!(
            matches!(
                waiter.try_recv(),
                Err(mpsc::error::TryRecvError::Disconnected)
            ),
            "waiters are released when the connection closes"
        );

        let mut late = router.register(71, PendingKind::Reply);
        assert!(
            matches!(
                late.try_recv(),
                Err(mpsc::error::TryRecvError::Disconnected)
            ),
            "a waiter registered after close fails immediately"
        );
    }

    /// A `History` that reports our own running turn as active must not be
    /// mirrored as a peer turn.
    #[test]
    fn history_active_turn_for_our_own_turn_is_not_mirrored() {
        let mut router = lifecycle_router();
        let _prompt = router.register(77, PendingKind::Prompt);
        router.route(turn_started("t1", ours(77), Some("mine")));

        let actions = router.route(history_event(
            &["turn_lifecycle"],
            Some(json!({
                "turn_id": "t1",
                "origin": {
                    "kind": "client",
                    "client_instance_id": OURS,
                    "request_id": 77
                },
            })),
        ));
        assert!(
            actions.is_empty(),
            "our own active turn is not a peer turn: {actions:?}"
        );
    }
}
