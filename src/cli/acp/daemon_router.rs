//! Routing of daemon events on one ACP adapter connection.
//!
//! The adapter reads its daemon connection continuously (see
//! [`super::DaemonSession::spawn_reader`]) instead of only while one of its own
//! prompts is running. Every event is therefore routed here first:
//!
//! - replies to a request this adapter is waiting on go to that waiter;
//! - events scoped to a turn (`turn_started`, `turn_event`, `turn_ended`) are
//!   classified by the turn's identity *before* they are unwrapped: this
//!   adapter's own turn feeds its `session/prompt`, a peer turn is mirrored to
//!   the ACP client;
//! - session-wide state (model, effort, title) is applied whenever it arrives;
//! - anything else is drained, so a peer's turn can never leak into the next
//!   prompt's stream (the misattribution bug this design exists to fix).
//!
//! A daemon that advertises the `turn_origin` capability (learned from `Pong`
//! before subscribing) is read with turn identity; an older daemon is read
//! flat, with the legacy behaviour: events go to the running prompt or are
//! drained.
//!
//! The routing state is deliberately free of sockets and stdout so the whole
//! policy is unit-testable with scripted [`ServerEvent`] sequences.

use super::*;
use crate::protocol::{TurnEndStatus, TurnIdentity, TurnOrigin};
use std::collections::HashSet;

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

/// The canonical string id of a turn, used for `_jcode/turn` and peer
/// `messageId`s. The runtime generation is a fresh UUID per agent runtime, so
/// the id stays unique across daemon restarts.
pub(super) fn turn_id_string(identity: &TurnIdentity) -> String {
    format!("{}:{}", identity.runtime_generation, identity.turn_id)
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
    identity: TurnIdentity,
    request_id: u64,
}

/// A turn another attachment started, being mirrored to the ACP client.
#[derive(Debug, Clone)]
struct PeerTurn {
    identity: TurnIdentity,
    /// Its prompt, while the attach replay may still contain it (the turn was
    /// opened before this connection's first `History`).
    unreplayed_prompt: Option<String>,
}

/// Port of the TUI's turn fence (`tui/app/remote/turn_identity.rs`): retire
/// old runtime generations and reject turn ids at or below the completed
/// watermark. Turn ids may have gaps (debug paths claim ids without
/// publishing), so nothing here expects them to be contiguous.
///
/// Unlike the TUI, a `turn_started` from a generation this connection has not
/// seen (and has not retired) is adopted rather than dropped: the ACP adapter
/// has no other way to recover a prompt whose turn the daemon accepted.
#[derive(Debug, Default)]
struct TurnFence {
    generation: Option<String>,
    retired: HashSet<String>,
    latest: u64,
    completed: u64,
}

impl TurnFence {
    /// Apply an attach snapshot. `None` means the snapshot is stale and must
    /// be ignored; `Some(true)` means it replaced a known runtime.
    fn bootstrap(&mut self, generation: &str, latest: u64, active: Option<u64>) -> Option<bool> {
        if self.retired.contains(generation) {
            return None;
        }
        let replaced = self.adopt(generation);
        if latest < self.latest {
            return None;
        }
        self.latest = latest;
        if active.is_none() {
            self.completed = self.completed.max(latest);
        }
        Some(replaced)
    }

    /// Switch to `generation`, retiring the previous one. Returns whether a
    /// known runtime was replaced.
    fn adopt(&mut self, generation: &str) -> bool {
        if self.generation.as_deref() == Some(generation) {
            return false;
        }
        let replaced = match self.generation.replace(generation.to_string()) {
            Some(old) => {
                self.retired.insert(old);
                true
            }
            None => false,
        };
        self.latest = 0;
        self.completed = 0;
        replaced
    }

    /// Whether an active turn reported by a snapshot can still be opened.
    fn admits(&self, identity: &TurnIdentity) -> bool {
        self.generation.as_deref() == Some(identity.runtime_generation.as_str())
            && identity.turn_id > self.completed
    }

    /// Admit a `turn_started`. `None` rejects it; `Some(true)` means it
    /// replaced a known runtime.
    fn start(&mut self, identity: &TurnIdentity) -> Option<bool> {
        if self.retired.contains(&identity.runtime_generation) {
            return None;
        }
        let replaced = self.adopt(&identity.runtime_generation);
        if identity.turn_id < self.latest || identity.turn_id <= self.completed {
            return None;
        }
        self.latest = identity.turn_id;
        Some(replaced)
    }

    fn complete(&mut self, identity: &TurnIdentity) {
        if self.generation.as_deref() == Some(identity.runtime_generation.as_str()) {
            self.completed = self.completed.max(identity.turn_id);
        }
    }
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
        /// Canonical `"{runtime_generation}:{turn_id}"`.
        turn_id: String,
        origin: TurnOrigin,
        user_content: Option<String>,
        /// The attaching client's replayed `History` already contains this
        /// prompt, so re-sending it as a live user chunk would show it twice.
        prompt_already_replayed: bool,
    },
    /// The attach `History` turned out to contain the prompt of a peer turn
    /// announced before it. The adapter still holds that announcement (peer
    /// output is gated until the replay finished), so it drops the user chunk.
    PeerPromptReplayed {
        turn_id: String,
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
    /// The daemon advertised `turn_origin`: the connection subscribes with
    /// turn identity and routes on it. Otherwise events are read flat.
    turn_origin: bool,
    pending: HashMap<u64, PendingRequest>,
    own_turn: Option<OwnTurn>,
    peer_turn: Option<PeerTurn>,
    fence: TurnFence,
    /// Set by the first `History`: until then a peer turn's prompt may still
    /// show up in the attach replay.
    history_seen: bool,
    /// Set once the connection is gone: no new waiter can be satisfied, and an
    /// open peer turn is closed.
    closed: bool,
}

impl RouterState {
    pub(super) fn new(instance_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            session_id: session_id.into(),
            turn_origin: false,
            pending: HashMap::new(),
            own_turn: None,
            peer_turn: None,
            fence: TurnFence::default(),
            history_seen: false,
            closed: false,
        }
    }

    pub(super) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Record whether the daemon advertised `turn_origin`. Call before
    /// subscribing; the subscription's `supports_turn_identity` follows it.
    pub(super) fn set_turn_origin(&mut self, supported: bool) {
        self.turn_origin = supported;
    }

    pub(super) fn supports_turn_origin(&self) -> bool {
        self.turn_origin
    }

    /// Whether this adapter's own turn is running right now. A prompt the
    /// daemon has not accepted as a turn is *not* running.
    #[cfg(test)]
    pub(super) fn own_turn_running(&self) -> bool {
        self.own_turn.is_some()
    }

    /// Identity of this adapter's own running turn.
    pub(super) fn own_turn_identity(&self) -> Option<TurnIdentity> {
        self.own_turn.as_ref().map(|turn| turn.identity.clone())
    }

    /// Identity of the turn this connection sees running: its own, or the
    /// mirrored peer turn. A user stop targets exactly that turn.
    pub(super) fn active_turn_identity(&self) -> Option<TurnIdentity> {
        self.own_turn_identity()
            .or_else(|| self.peer_turn.as_ref().map(|turn| turn.identity.clone()))
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
        self.own_turn = None;
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
        if !self.turn_origin {
            return self.route_legacy(event);
        }
        match event {
            ServerEvent::TurnState {
                session_id,
                runtime_generation,
                turn_id,
                latest_turn_id,
                active_origin,
                active_user_content,
            } => self.on_turn_state(
                session_id,
                runtime_generation,
                turn_id,
                latest_turn_id,
                active_origin,
                active_user_content,
            ),
            ServerEvent::TurnStarted {
                identity,
                origin,
                user_content,
            } => self.on_turn_started(identity, origin, user_content),
            ServerEvent::TurnEnded {
                identity,
                status,
                message,
            } => self.on_turn_ended(identity, status, message),
            ServerEvent::TurnEvent { identity, event } => self.on_scoped(identity, *event),
            event => self.on_flat(event),
        }
    }

    /// `turn_state` precedes every `History`: it says which turn, if any, is
    /// running right now. A running peer turn this connection has not seen
    /// start (a mid-turn attach) is opened here.
    fn on_turn_state(
        &mut self,
        session_id: String,
        runtime_generation: String,
        turn_id: Option<u64>,
        latest_turn_id: u64,
        active_origin: Option<TurnOrigin>,
        active_user_content: Option<String>,
    ) -> Vec<RouteAction> {
        let Some(replaced) = self
            .fence
            .bootstrap(&runtime_generation, latest_turn_id, turn_id)
        else {
            crate::logging::debug("acp: ignoring a stale turn_state");
            return Vec::new();
        };
        self.session_id = session_id;
        let mut actions = Vec::new();
        if replaced {
            actions.extend(self.retire_runtime());
        }
        let (Some(turn_id), Some(origin)) = (turn_id, active_origin) else {
            // No active turn, or one nobody announces (a debug path that
            // claims a turn id without publishing): nothing to mirror.
            return actions;
        };
        let identity = TurnIdentity {
            runtime_generation,
            turn_id,
        };
        if !self.fence.admits(&identity) || self.is_open(&identity) {
            return actions;
        }
        if self.is_own_origin(&origin) {
            // A snapshot taken during our own prompt (a catalog refresh) whose
            // `turn_started` this connection somehow missed.
            self.own_turn = Some(OwnTurn {
                request_id: origin.request_id.unwrap_or_default(),
                identity,
            });
            return actions;
        }
        actions.extend(self.close_peer_turn(
            TurnEndStatus::Cancelled,
            Some("The turn ended without a lifecycle end event.".to_string()),
        ));
        actions.extend(self.open_peer_turn(identity, origin, active_user_content));
        actions
    }

    fn on_turn_started(
        &mut self,
        identity: TurnIdentity,
        origin: Option<TurnOrigin>,
        user_content: Option<String>,
    ) -> Vec<RouteAction> {
        if self.is_open(&identity) {
            // Already opened from an attach snapshot.
            return Vec::new();
        }
        let Some(replaced) = self.fence.start(&identity) else {
            crate::logging::debug(&format!(
                "acp: dropping turn_started for fenced turn {}",
                turn_id_string(&identity)
            ));
            return Vec::new();
        };
        let mut actions = Vec::new();
        if replaced {
            actions.extend(self.retire_runtime());
        }
        // Turns never overlap within a runtime, so a peer turn still open here
        // never reported its end (an aborted or panicking peer). Close it
        // before opening anything else, or the new turn's stream would be
        // mirrored as that stale peer turn's output.
        actions.extend(self.close_peer_turn(
            TurnEndStatus::Cancelled,
            Some("The turn ended without a lifecycle end event.".to_string()),
        ));
        // A daemon advertising `turn_origin` always sends one; without it the
        // turn cannot be ours.
        let origin = origin
            .unwrap_or_else(|| TurnOrigin::server(crate::protocol::TurnOriginKind::Background));
        if self.is_own_origin(&origin) {
            self.own_turn = Some(OwnTurn {
                request_id: origin.request_id.unwrap_or_default(),
                identity,
            });
        } else {
            actions.extend(self.open_peer_turn(identity, origin, user_content));
        }
        actions
    }

    fn on_turn_ended(
        &mut self,
        identity: TurnIdentity,
        status: TurnEndStatus,
        message: Option<String>,
    ) -> Vec<RouteAction> {
        if let Some(own) = self
            .own_turn
            .as_ref()
            .filter(|own| own.identity == identity)
        {
            // Keep `own_turn` until the scoped terminal: it is the prompt's
            // completion. A cancelled turn tells the prompt so, because the
            // cancel path's own `interrupted` reaches this connection only
            // after the turn's terminal (and only if this connection
            // cancelled).
            if status == TurnEndStatus::Cancelled {
                let request_id = own.request_id;
                self.deliver(request_id, ServerEvent::Interrupted);
            }
            return Vec::new();
        }
        if self.peer_matches(&identity) {
            self.fence.complete(&identity);
            return self.close_peer_turn(status, message);
        }
        crate::logging::debug(&format!(
            "acp: dropping turn_ended for a turn that is not open: {}",
            turn_id_string(&identity)
        ));
        Vec::new()
    }

    /// An event inside a turn's scope. It belongs to that turn and nothing
    /// else: the own prompt, the mirrored peer turn, or (for a turn this
    /// connection does not have open, such as a peer's second terminal after a
    /// forced abort) nobody.
    fn on_scoped(&mut self, identity: TurnIdentity, event: ServerEvent) -> Vec<RouteAction> {
        let event = match flatten_scope(event) {
            Some(event) => event,
            None => {
                crate::logging::debug("acp: dropping a nested turn scope event");
                return Vec::new();
            }
        };
        let terminal = matches!(event, ServerEvent::Done { .. } | ServerEvent::Error { .. });
        if let Some(own) = self
            .own_turn
            .as_ref()
            .filter(|own| own.identity == identity)
        {
            let request_id = own.request_id;
            if terminal {
                self.own_turn = None;
                self.fence.complete(&identity);
            } else if let Some(change) = session_state_change(&event) {
                return vec![RouteAction::SessionState(change)];
            }
            self.deliver(request_id, event);
            return Vec::new();
        }
        if self.peer_matches(&identity) {
            if terminal {
                // Normally `turn_ended` closed the turn already; this is a
                // terminal without one (an older daemon build).
                self.fence.complete(&identity);
                let (status, message) = match &event {
                    ServerEvent::Error { message, .. } => {
                        (TurnEndStatus::Failed, Some(message.clone()))
                    }
                    _ => (TurnEndStatus::Completed, None),
                };
                return self.close_peer_turn(status, message);
            }
            if let Some(change) = session_state_change(&event) {
                return vec![RouteAction::SessionState(change)];
            }
            return vec![RouteAction::PeerTurnEvent(event)];
        }
        crate::logging::debug(&format!(
            "acp: dropping an event scoped to a turn that is not open: {}",
            turn_id_string(&identity)
        ));
        Vec::new()
    }

    /// An unscoped event: a reply, session state, or (rarely) a notice the
    /// daemon does not scope.
    fn on_flat(&mut self, event: ServerEvent) -> Vec<RouteAction> {
        let mut actions = Vec::new();
        if let ServerEvent::History { .. } = &event {
            actions.extend(self.on_history(&event));
        }

        if let Some(request_id) = reply_request_id(&event) {
            let terminal = matches!(event, ServerEvent::Done { .. } | ServerEvent::Error { .. });
            if self.pending.contains_key(&request_id) {
                // An unscoped terminal is a reply to a request that never
                // became a turn: a rejected prompt ("Already processing a
                // message") or a slash command.
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
            // Superseded by `turn_started.user_content`, which has no ordering
            // race with the turn.
            ServerEvent::PeerUserMessage { .. } => {}
            event => {
                if let Some(change) = session_state_change(&event) {
                    actions.push(RouteAction::SessionState(change));
                    return actions;
                }
                if let Some(own) = self.own_turn.as_ref() {
                    let request_id = own.request_id;
                    self.deliver(request_id, event);
                } else if self.peer_turn.is_some() {
                    actions.push(RouteAction::PeerTurnEvent(event));
                } else {
                    crate::logging::debug("acp: dropping daemon event with no open turn");
                }
            }
        }
        actions
    }

    /// Session metadata from a `History`. The first one is the attach
    /// snapshot: a peer turn announced before it whose prompt it already
    /// contains must not show that prompt again.
    fn on_history(&mut self, event: &ServerEvent) -> Vec<RouteAction> {
        let ServerEvent::History {
            session_id,
            messages,
            ..
        } = event
        else {
            return Vec::new();
        };
        self.session_id = session_id.clone();
        self.history_seen = true;
        let Some(peer) = self.peer_turn.as_mut() else {
            return Vec::new();
        };
        match peer.unreplayed_prompt.take() {
            Some(content) if history_has_prompt(messages, &content) => {
                vec![RouteAction::PeerPromptReplayed {
                    turn_id: turn_id_string(&peer.identity),
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Flat routing for a daemon without `turn_origin`: there are no turn
    /// boundaries, so keep the original behaviour for our own prompt and drain
    /// anything that arrives while idle.
    fn route_legacy(&mut self, event: ServerEvent) -> Vec<RouteAction> {
        let Some(event) = flatten_scope(event) else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        if let ServerEvent::History { session_id, .. } = &event {
            self.session_id = session_id.clone();
            self.history_seen = true;
        }
        if let Some(request_id) = reply_request_id(&event) {
            if self.pending.contains_key(&request_id) {
                self.deliver(request_id, event);
                return actions;
            }
            if !matches!(event, ServerEvent::ModelChanged { .. }) {
                crate::logging::debug(&format!(
                    "acp: dropping reply for non-pending request id {request_id}"
                ));
                return actions;
            }
        }
        match event {
            ServerEvent::PeerUserMessage { .. } => {}
            event => {
                if let Some(change) = session_state_change(&event) {
                    actions.push(RouteAction::SessionState(change));
                    return actions;
                }
                if let Some(prompt_id) = self.prompt_request_id() {
                    self.deliver(prompt_id, event);
                } else {
                    crate::logging::debug("acp: draining idle daemon event");
                }
            }
        }
        actions
    }

    fn is_open(&self, identity: &TurnIdentity) -> bool {
        self.own_turn
            .as_ref()
            .is_some_and(|own| &own.identity == identity)
            || self.peer_matches(identity)
    }

    fn peer_matches(&self, identity: &TurnIdentity) -> bool {
        self.peer_turn
            .as_ref()
            .is_some_and(|peer| &peer.identity == identity)
    }

    /// The runtime this connection watched was replaced: its turns can never
    /// end on this connection.
    fn retire_runtime(&mut self) -> Vec<RouteAction> {
        if let Some(own) = self.own_turn.take() {
            self.deliver(
                own.request_id,
                ServerEvent::Error {
                    id: own.request_id,
                    message: "The session's runtime was replaced before the turn ended."
                        .to_string(),
                    retry_after_secs: None,
                },
            );
        }
        self.close_peer_turn(
            TurnEndStatus::Cancelled,
            Some("The session's runtime was replaced before the turn ended.".to_string()),
        )
    }

    fn open_peer_turn(
        &mut self,
        identity: TurnIdentity,
        origin: TurnOrigin,
        user_content: Option<String>,
    ) -> Vec<RouteAction> {
        let unreplayed_prompt = if self.history_seen {
            None
        } else {
            user_content.clone()
        };
        let turn_id = turn_id_string(&identity);
        self.peer_turn = Some(PeerTurn {
            identity,
            unreplayed_prompt,
        });
        vec![RouteAction::PeerTurnStarted {
            turn_id,
            origin,
            user_content,
            prompt_already_replayed: false,
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
            turn_id: turn_id_string(&peer.identity),
            status,
            message,
        }]
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

/// The event inside (possibly nested) `turn_event` scopes, or `None` for a
/// nested lifecycle event, which would bypass the scope check.
fn flatten_scope(mut event: ServerEvent) -> Option<ServerEvent> {
    loop {
        event = match event {
            ServerEvent::TurnEvent { event, .. } => *event,
            ServerEvent::TurnStarted { .. }
            | ServerEvent::TurnEnded { .. }
            | ServerEvent::TurnState { .. } => return None,
            event => return Some(event),
        };
    }
}

/// How many trailing history messages are searched for the running turn's
/// prompt. The prompt is appended once per turn, so it is near the tail; the
/// bound keeps the check cheap on a long transcript and avoids matching a much
/// older message that happens to share its text.
const PROMPT_MATCH_WINDOW: usize = 16;

/// Whether replayed `History` already contains `content` as a user message.
fn history_has_prompt(messages: &[crate::protocol::HistoryMessage], content: &str) -> bool {
    messages
        .iter()
        .rev()
        .take(PROMPT_MATCH_WINDOW)
        .any(|message| message.role == "user" && message.content == content)
}

/// Request ids that answer a request this adapter may be waiting on.
fn reply_request_id(event: &ServerEvent) -> Option<u64> {
    match event {
        ServerEvent::Ack { id }
        | ServerEvent::Done { id }
        | ServerEvent::Error { id, .. }
        | ServerEvent::Pong { id, .. }
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
#[path = "daemon_router_tests.rs"]
mod tests;
