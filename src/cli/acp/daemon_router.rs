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
//! A daemon that also advertises `soft_interrupt_ids` is *steered*: a prompt is
//! sent as a soft interrupt, so it starts a turn on an idle session and is
//! injected into the running turn otherwise (see [`PromptDelivery`]). The
//! router tracks each prompt until a turn takes it, recovers a steer left in
//! the daemon's queue when the turn it waited for ended, and hands a peer turn
//! that took our steer over to the prompt.
//!
//! The routing state is deliberately free of sockets and stdout so the whole
//! policy is unit-testable with scripted [`ServerEvent`] sequences. Requests
//! the policy needs sent come back as [`RouteAction::Send`], timers as
//! [`RouteAction::Wake`].

use super::*;
use crate::protocol::{TurnEndStatus, TurnIdentity, TurnOrigin};
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

/// The daemon's reply when a turn cannot start because another one (or a
/// runtime mutation) holds the session.
const ALREADY_PROCESSING: &str = "Already processing a message";

/// Backoff before resending a steer the daemon rejected as busy, like the
/// TUI's queued follow-up.
pub(super) const STEER_RETRY_DELAY: Duration = Duration::from_millis(100);

/// How long a steer sent while no turn is visible may sit in the daemon's
/// queue before the router asks whether it is stranded there (the daemon
/// queues a steer while the previous turn is still finalizing).
pub(super) const STEER_IDLE_PROBE_DELAY: Duration = Duration::from_millis(300);

/// How long to wait for the injection event of a steer the daemon reported as
/// no longer queued. If none arrives, the queue was cleared by someone else.
pub(super) const STEER_CONFIRM_GRACE: Duration = Duration::from_secs(5);

/// Upper bound on resends of one prompt, so a session that never becomes free
/// fails the prompt instead of retrying forever (about 30 s of 100 ms
/// retries).
const MAX_STEER_ATTEMPTS: u32 = 300;

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
    /// A catalog History omits live-session and turn snapshots.
    Catalog,
}

pub(super) struct PendingRequest {
    kind: PendingKind,
    tx: mpsc::UnboundedSender<ServerEvent>,
}

/// A turn that answers this adapter's prompts.
#[derive(Debug, Clone)]
struct OwnTurn {
    identity: TurnIdentity,
    /// The prompt that receives the turn's stream and answers with its usage.
    request_id: u64,
    /// Further prompts steered into the turn. They resolve when it ends.
    joined: Vec<u64>,
    /// The turn was started by another client and taken over when our steer
    /// was injected into it. Close never cancels it.
    adopted: bool,
    /// The turn was announced with a `_jcode/turn` "started" while it was
    /// mirrored as a peer turn, and its terminal bracket is still owed.
    bracket_open: bool,
}

impl OwnTurn {
    fn started(identity: TurnIdentity, request_id: u64) -> Self {
        Self {
            identity,
            request_id,
            joined: Vec::new(),
            adopted: false,
            bracket_open: false,
        }
    }

    fn members(&self) -> impl Iterator<Item = u64> + '_ {
        std::iter::once(self.request_id).chain(self.joined.iter().copied())
    }
}

/// A turn another attachment started, being mirrored to the ACP client.
#[derive(Debug, Clone)]
struct PeerTurn {
    identity: TurnIdentity,
    /// Its prompt, while the attach replay may still contain it (the turn was
    /// opened before this connection's first `History`).
    unreplayed_prompt: Option<String>,
    /// Steers other clients injected into it so far; numbers their messages.
    steers: u32,
}

/// How a prompt reaches the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptDelivery {
    /// `Message`: starts a turn on an idle session and is rejected on a busy
    /// one. Used by a legacy daemon, and first for a prompt that carries a
    /// skill selection (a soft interrupt cannot), falling back to a steer
    /// when the session is busy.
    Message,
    /// `SoftInterrupt` with a client id: starts a turn on an idle session and
    /// is injected into the running turn otherwise.
    Steer,
}

/// Where a prompt that no turn has taken yet stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitPhase {
    /// Sent; the daemon started a turn with it, queued it, or rejected it.
    Sent,
    /// A `CancelSoftInterrupts` with this request id asks whether it is still
    /// queued.
    Probing(u64),
    /// Rejected as busy; resent after a backoff or at the next turn end.
    RetryWait,
    /// The daemon no longer had it queued, so a turn took it; its injection
    /// event has not arrived yet.
    Unconfirmed,
}

/// Why a waiting prompt is being withdrawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Withdrawal {
    Stop,
    Close,
}

/// A prompt no turn has taken yet.
#[derive(Debug, Clone)]
struct WaitingPrompt {
    client_id: String,
    content: String,
    images: Vec<(String, String)>,
    delivery: PromptDelivery,
    phase: WaitPhase,
    withdrawal: Option<Withdrawal>,
    attempts: u32,
}

/// A timer the router asked for; see [`RouterState::wake`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WakeKind {
    /// Ask whether a steer sent while no turn was visible is stranded.
    Probe,
    /// Resend a steer rejected as busy.
    Retry,
    /// Give up on the injection event of a steer no longer queued.
    Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SteerWake {
    pub(super) prompt_id: u64,
    pub(super) kind: WakeKind,
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
        reasoning_effort: Option<String>,
        available_reasoning_efforts: Option<Vec<String>>,
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
    /// Another client steered the mirrored peer turn: show its message.
    PeerSteer {
        turn_id: String,
        index: u32,
        content: String,
    },
    /// Our steer was injected into the mirrored peer turn, which now answers
    /// that prompt: stop mirroring it. Its `_jcode/turn` terminal is written by
    /// the prompt, after the rest of its output.
    PeerTurnAdopted {
        turn_id: String,
    },
    SessionState(SessionStateChange),
    /// Send this request to the daemon.
    Send(Request),
    /// Call [`RouterState::wake`] with `token` once `after` has passed.
    Wake {
        after: Duration,
        token: SteerWake,
    },
}

/// Routing state for one daemon connection.
pub(super) struct RouterState {
    instance_id: String,
    session_id: String,
    /// The daemon advertised `turn_origin`: the connection subscribes with
    /// turn identity and routes on it. Otherwise events are read flat.
    turn_origin: bool,
    live_sessions: Option<Vec<String>>,
    pending: HashMap<u64, PendingRequest>,
    correlations: HashMap<u64, (String, &'static str)>,
    own_turn: Option<OwnTurn>,
    peer_turn: Option<PeerTurn>,
    fence: TurnFence,
    /// Set by the first `History`: until then a peer turn's prompt may still
    /// show up in the attach replay.
    history_seen: bool,
    /// Set once the connection is gone: no new waiter can be satisfied, and an
    /// open peer turn is closed.
    closed: bool,
    /// The daemon accepts client ids on soft interrupts (`soft_interrupt_ids`).
    soft_interrupt_ids: bool,
    /// Request ids, shared with the connection so router-built requests never
    /// collide with the adapter's own.
    request_ids: Arc<AtomicU64>,
    /// Prompts no turn has taken yet, by request id.
    waiting: BTreeMap<u64, WaitingPrompt>,
    /// Outstanding `CancelSoftInterrupts` request ids.
    probes: HashSet<u64>,
    /// `session/close` ran: nothing may start or adopt a turn for this view.
    view_closed: bool,
}

impl RouterState {
    pub(super) fn new(instance_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            session_id: session_id.into(),
            turn_origin: false,
            live_sessions: None,
            pending: HashMap::new(),
            correlations: HashMap::new(),
            own_turn: None,
            peer_turn: None,
            fence: TurnFence::default(),
            history_seen: false,
            closed: false,
            soft_interrupt_ids: false,
            request_ids: Arc::new(AtomicU64::new(1 << 62)),
            waiting: BTreeMap::new(),
            probes: HashSet::new(),
            view_closed: false,
        }
    }

    /// Share the connection's request-id counter.
    pub(super) fn share_request_ids(&mut self, request_ids: Arc<AtomicU64>) {
        self.request_ids = request_ids;
    }

    fn next_request_id(&self) -> u64 {
        self.request_ids.fetch_add(1, Ordering::Relaxed)
    }

    /// Record whether the daemon accepts client ids on soft interrupts.
    pub(super) fn set_soft_interrupt_ids(&mut self, supported: bool) {
        self.soft_interrupt_ids = supported;
    }

    /// Whether prompts are steered: sent as soft interrupts that join a
    /// running turn instead of being rejected by it.
    pub(super) fn steering(&self) -> bool {
        self.turn_origin && self.soft_interrupt_ids
    }

    /// The soft-interrupt client id of prompt `prompt_id`.
    pub(super) fn steer_client_id(&self, prompt_id: u64) -> String {
        format!("{}/p{prompt_id}", self.instance_id)
    }

    pub(super) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Live session ids reported by the daemon, once any `History` arrived.
    pub(super) fn live_sessions(&self) -> Option<&[String]> {
        self.live_sessions.as_deref()
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

    /// Identity of the turn answering this adapter's prompts, including a
    /// peer turn it adopted.
    pub(super) fn own_turn_identity(&self) -> Option<TurnIdentity> {
        self.own_turn.as_ref().map(|turn| turn.identity.clone())
    }

    /// Whether any turn is open on this view.
    fn turn_open(&self) -> bool {
        self.own_turn.is_some() || self.peer_turn.is_some()
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

    pub(super) fn prompt_correlation(&self, request_id: u64) -> Option<(String, &'static str)> {
        self.correlations.get(&request_id).cloned()
    }

    pub(super) fn unregister(&mut self, request_id: u64) {
        self.correlations.remove(&request_id);
        self.pending.remove(&request_id);
        self.waiting.remove(&request_id);
    }

    /// Track prompt `prompt_id` (already registered as a [`PendingKind::Prompt`])
    /// until a turn takes it. Only meaningful when [`Self::steering`].
    pub(super) fn track_prompt(
        &mut self,
        prompt_id: u64,
        delivery: PromptDelivery,
        content: String,
        images: Vec<(String, String)>,
    ) {
        if self.closed || !self.steering() {
            return;
        }
        let client_id = self.steer_client_id(prompt_id);
        self.waiting.insert(
            prompt_id,
            WaitingPrompt {
                client_id,
                content,
                images,
                delivery,
                phase: WaitPhase::Sent,
                withdrawal: None,
                attempts: 0,
            },
        );
    }

    /// The soft interrupt that steers prompt `prompt_id`.
    pub(super) fn steer_request(&self, prompt_id: u64) -> Option<Request> {
        let waiting = self.waiting.get(&prompt_id)?;
        Some(Request::SoftInterrupt {
            id: prompt_id,
            content: waiting.content.clone(),
            images: waiting.images.clone(),
            urgent: false,
            client_id: Some(waiting.client_id.clone()),
        })
    }

    /// Call after prompt `prompt_id` was sent. A steer sent while no turn is
    /// visible may still be queued behind a turn that is finalizing, which no
    /// turn end will report, so it is checked on a timer.
    pub(super) fn prompt_sent(&mut self, prompt_id: u64) -> Vec<RouteAction> {
        let idle = !self.turn_open();
        match self.waiting.get(&prompt_id) {
            Some(waiting)
                if idle
                    && waiting.delivery == PromptDelivery::Steer
                    && waiting.phase == WaitPhase::Sent =>
            {
                vec![RouteAction::Wake {
                    after: STEER_IDLE_PROBE_DELAY,
                    token: SteerWake {
                        prompt_id,
                        kind: WakeKind::Probe,
                    },
                }]
            }
            _ => Vec::new(),
        }
    }

    /// A timer requested by a [`RouteAction::Wake`] fired.
    pub(super) fn wake(&mut self, token: SteerWake) -> Vec<RouteAction> {
        let Some(waiting) = self.waiting.get(&token.prompt_id) else {
            return Vec::new();
        };
        match (token.kind, waiting.phase) {
            // While a turn is open, its end probes instead.
            (WakeKind::Probe, WaitPhase::Sent) if !self.turn_open() => {
                self.probe(vec![token.prompt_id])
            }
            (WakeKind::Retry, WaitPhase::RetryWait) => self.resend(token.prompt_id),
            (WakeKind::Confirm, WaitPhase::Unconfirmed) => {
                // The daemon had already dropped it and no turn ever took it:
                // another client cleared the session's queue.
                crate::logging::debug(&format!(
                    "acp: steer {} left the daemon queue without running",
                    token.prompt_id
                ));
                self.resolve_cancelled(token.prompt_id);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// `session/cancel`: withdraw every prompt still waiting for a turn. The
    /// caller still cancels the running turn.
    pub(super) fn stop(&mut self) -> Vec<RouteAction> {
        self.withdraw(Withdrawal::Stop)
    }

    /// `session/close`: withdraw waiting prompts, cancel this view's own
    /// running turn, and release an adopted peer turn without cancelling it.
    /// From now on nothing starts or adopts a turn for this view.
    pub(super) fn close_view(&mut self) -> Vec<RouteAction> {
        self.view_closed = true;
        let mut actions = self.withdraw(Withdrawal::Close);
        actions.extend(self.close_peer_turn(TurnEndStatus::Cancelled, None));
        let Some(own) = self.own_turn.as_ref() else {
            return actions;
        };
        if !own.adopted {
            // Scoped, so a cancel that arrives after the turn ended cannot hit
            // a peer's turn that began in the meantime.
            actions.push(RouteAction::Send(Request::Cancel {
                id: self.next_request_id(),
                turn: Some(own.identity.clone()),
            }));
            return actions;
        }
        // Another client started it: let it run, but stop answering our
        // prompts with it. Its remaining events are dropped as not open.
        let own = self.own_turn.take().expect("checked above");
        if own.bracket_open {
            self.deliver(
                own.request_id,
                ServerEvent::TurnEnded {
                    identity: own.identity.clone(),
                    status: TurnEndStatus::Cancelled,
                    message: Some(
                        "The session was closed; the turn continues for the client that started it."
                            .to_string(),
                    ),
                },
            );
        }
        for member in own.members() {
            self.deliver(member, ServerEvent::Interrupted);
            self.deliver(member, ServerEvent::Done { id: member });
        }
        actions
    }

    fn withdraw(&mut self, reason: Withdrawal) -> Vec<RouteAction> {
        let mut probe = Vec::new();
        let mut resolve = Vec::new();
        for (prompt_id, waiting) in self.waiting.iter_mut() {
            waiting.withdrawal = Some(reason);
            match waiting.phase {
                WaitPhase::Sent if waiting.delivery == PromptDelivery::Steer => {
                    probe.push(*prompt_id)
                }
                // A `Message` in flight becomes a turn (cancelled by the
                // caller's stop, or by the close rule) or a rejection.
                WaitPhase::Sent => {}
                // The outstanding probe's reply decides.
                WaitPhase::Probing(_) => {}
                // Nothing is queued on the daemon.
                WaitPhase::RetryWait | WaitPhase::Unconfirmed => resolve.push(*prompt_id),
            }
        }
        for prompt_id in resolve {
            self.resolve_cancelled(prompt_id);
        }
        if probe.is_empty() {
            return Vec::new();
        }
        self.probe(probe)
    }

    /// Ask the daemon to remove these prompts' steers from its queue. The
    /// decision is made under the queue lock: each comes back either
    /// cancelled (still queued) or not queued (a turn took it).
    fn probe(&mut self, prompt_ids: Vec<u64>) -> Vec<RouteAction> {
        let id = self.next_request_id();
        let mut client_ids = Vec::new();
        for prompt_id in prompt_ids {
            if let Some(waiting) = self.waiting.get_mut(&prompt_id) {
                waiting.phase = WaitPhase::Probing(id);
                client_ids.push(waiting.client_id.clone());
            }
        }
        if client_ids.is_empty() {
            return Vec::new();
        }
        self.probes.insert(id);
        vec![RouteAction::Send(Request::CancelSoftInterrupts {
            id,
            client_ids: Some(client_ids),
        })]
    }

    /// Send prompt `prompt_id` again as a steer.
    fn resend(&mut self, prompt_id: u64) -> Vec<RouteAction> {
        let Some(waiting) = self.waiting.get_mut(&prompt_id) else {
            return Vec::new();
        };
        waiting.attempts += 1;
        if waiting.attempts > MAX_STEER_ATTEMPTS {
            self.waiting.remove(&prompt_id);
            self.deliver(
                prompt_id,
                ServerEvent::Error {
                    id: prompt_id,
                    message: format!("{ALREADY_PROCESSING}; the session stayed busy"),
                    retry_after_secs: None,
                },
            );
            return Vec::new();
        }
        waiting.delivery = PromptDelivery::Steer;
        waiting.phase = WaitPhase::Sent;
        let mut actions: Vec<RouteAction> = self
            .steer_request(prompt_id)
            .map(RouteAction::Send)
            .into_iter()
            .collect();
        actions.extend(self.prompt_sent(prompt_id));
        actions
    }

    /// Answer a withdrawn (or lost) prompt as cancelled.
    fn resolve_cancelled(&mut self, prompt_id: u64) {
        self.waiting.remove(&prompt_id);
        self.deliver(prompt_id, ServerEvent::Interrupted);
        self.deliver(prompt_id, ServerEvent::Done { id: prompt_id });
    }

    /// Take the waiting prompts whose steers `client_ids` names: a turn took
    /// them. Returns their request ids in injection order.
    fn take_waiting(&mut self, client_ids: &[String]) -> Vec<u64> {
        let mut taken = Vec::new();
        for client_id in client_ids {
            let found = self
                .waiting
                .iter()
                .find_map(|(id, waiting)| (&waiting.client_id == client_id).then_some(*id));
            if let Some(prompt_id) = found {
                self.waiting.remove(&prompt_id);
                taken.push(prompt_id);
            }
        }
        taken
    }

    /// A turn ended. Steers still waiting were not injected into it: a
    /// rejected one is resent, and the daemon is asked about the others,
    /// because nothing drains a steer left queued when its turn ends.
    fn on_turn_closed(&mut self) -> Vec<RouteAction> {
        let mut probe = Vec::new();
        let mut retry = Vec::new();
        for (prompt_id, waiting) in &self.waiting {
            match waiting.phase {
                WaitPhase::Sent if waiting.delivery == PromptDelivery::Steer => {
                    probe.push(*prompt_id)
                }
                WaitPhase::RetryWait => retry.push(*prompt_id),
                _ => {}
            }
        }
        let mut actions = self.probe(probe);
        for prompt_id in retry {
            actions.extend(self.resend(prompt_id));
        }
        actions
    }

    /// The reply to a `CancelSoftInterrupts` this router sent.
    fn on_probe_reply(
        &mut self,
        id: u64,
        cancelled: &[String],
        not_queued: &[String],
    ) -> Vec<RouteAction> {
        let probing = |waiting: &WaitingPrompt| waiting.phase == WaitPhase::Probing(id);
        let mut actions = Vec::new();
        for client_id in cancelled {
            let Some((prompt_id, withdrawal)) = self.waiting.iter().find_map(|(prompt_id, w)| {
                (&w.client_id == client_id && probing(w)).then_some((*prompt_id, w.withdrawal))
            }) else {
                continue;
            };
            if withdrawal.is_some() {
                self.resolve_cancelled(prompt_id);
            } else {
                // Stranded behind a turn that ended: send it again, to start
                // a turn of its own or join one that just began.
                actions.extend(self.resend(prompt_id));
            }
        }
        for client_id in not_queued {
            let found = self.waiting.iter().find_map(|(prompt_id, w)| {
                (&w.client_id == client_id && probing(w)).then_some(*prompt_id)
            });
            if let Some(prompt_id) = found {
                if let Some(waiting) = self.waiting.get_mut(&prompt_id) {
                    waiting.phase = WaitPhase::Unconfirmed;
                }
                actions.push(RouteAction::Wake {
                    after: STEER_CONFIRM_GRACE,
                    token: SteerWake {
                        prompt_id,
                        kind: WakeKind::Confirm,
                    },
                });
            }
        }
        actions
    }

    /// An unscoped "Already processing" for a waiting prompt: the daemon could
    /// not start or queue it right now. Returns `None` for any other error.
    fn on_rejected(&mut self, prompt_id: u64, message: &str) -> Option<Vec<RouteAction>> {
        if message != ALREADY_PROCESSING {
            return None;
        }
        let waiting = self.waiting.get_mut(&prompt_id)?;
        if waiting.withdrawal.is_some() {
            self.resolve_cancelled(prompt_id);
            return Some(Vec::new());
        }
        match waiting.delivery {
            // A busy session: steer into its turn instead.
            PromptDelivery::Message => Some(self.resend(prompt_id)),
            PromptDelivery::Steer => {
                waiting.phase = WaitPhase::RetryWait;
                Some(vec![RouteAction::Wake {
                    after: STEER_RETRY_DELAY,
                    token: SteerWake {
                        prompt_id,
                        kind: WakeKind::Retry,
                    },
                }])
            }
        }
    }

    /// Drop every waiter so tasks blocked on a channel learn the connection is
    /// gone, and close any peer turn still open. Returns the actions the
    /// adapter must still render (the peer turn's end).
    pub(super) fn close(&mut self) -> Vec<RouteAction> {
        self.closed = true;
        self.pending.clear();
        self.waiting.clear();
        self.probes.clear();
        let message = "The connection closed before the turn ended.";
        let mut actions = self.own_turn.take().map_or_else(Vec::new, |own| {
            Self::adopted_bracket_end(&own, Some(message.to_string()))
        });
        actions.extend(self.close_peer_turn(TurnEndStatus::Cancelled, Some(message.to_string())));
        actions
    }

    /// The `_jcode/turn` terminal still owed for an adopted turn that ends
    /// without answering its prompt (the connection or runtime went away).
    fn adopted_bracket_end(own: &OwnTurn, message: Option<String>) -> Vec<RouteAction> {
        if !own.bracket_open {
            return Vec::new();
        }
        vec![RouteAction::PeerTurnEnded {
            turn_id: turn_id_string(&own.identity),
            status: TurnEndStatus::Cancelled,
            message,
        }]
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
            let request_id = origin.request_id.unwrap_or_default();
            self.waiting.remove(&request_id);
            self.correlations
                .insert(request_id, (turn_id_string(&identity), "origin"));
            self.own_turn = Some(OwnTurn::started(identity, request_id));
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
            // Our prompt started this turn (a `Message`, or a steer the daemon
            // ran at once because the session was idle).
            let request_id = origin.request_id.unwrap_or_default();
            self.waiting.remove(&request_id);
            if self.view_closed {
                // Sent before `session/close` and started after it: the close
                // rule cancels this view's own turn.
                actions.push(RouteAction::Send(Request::Cancel {
                    id: self.next_request_id(),
                    turn: Some(identity.clone()),
                }));
            }
            self.correlations
                .insert(request_id, (turn_id_string(&identity), "origin"));
            self.own_turn = Some(OwnTurn::started(identity, request_id));
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
            let members: Vec<u64> = own.members().collect();
            let primary = own.request_id;
            if status == TurnEndStatus::Cancelled {
                for member in members {
                    self.deliver(member, ServerEvent::Interrupted);
                }
            }
            self.end_adopted_bracket(primary, identity, status, message);
            return Vec::new();
        }
        if self.peer_matches(&identity) {
            self.fence.complete(&identity);
            let mut actions = self.close_peer_turn(status, message);
            actions.extend(self.on_turn_closed());
            return actions;
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
        if let ServerEvent::SoftInterruptInjected {
            content,
            client_ids,
            ..
        } = &event
        {
            let (content, client_ids) = (content.clone(), client_ids.clone());
            return self.on_injected(identity, event, content, &client_ids);
        }
        let terminal = matches!(event, ServerEvent::Done { .. } | ServerEvent::Error { .. });
        if let Some(own) = self
            .own_turn
            .as_ref()
            .filter(|own| own.identity == identity)
        {
            let request_id = own.request_id;
            if terminal {
                let own = self.own_turn.take().expect("matched above");
                self.fence.complete(&identity);
                // Without a `turn_ended` (an older daemon build) the adopted
                // turn's bracket is still open.
                if own.bracket_open {
                    let (status, message) = terminal_status(&event);
                    self.deliver(
                        own.request_id,
                        ServerEvent::TurnEnded {
                            identity,
                            status,
                            message,
                        },
                    );
                }
                // Every prompt the turn answers resolves with it; a terminal
                // carries the id of the request that started the turn.
                for member in own.members() {
                    self.deliver(member, retarget_terminal(&event, member));
                }
                return self.on_turn_closed();
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
                let (status, message) = terminal_status(&event);
                let mut actions = self.close_peer_turn(status, message);
                actions.extend(self.on_turn_closed());
                return actions;
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

    /// A steer was injected into turn `identity`.
    ///
    /// Ours (its client ids name a waiting prompt) joins our own turn, or
    /// makes a mirrored peer turn ours. Anyone else's is shown as a user
    /// message: a peer turn's by the mirror, our own turn's by its prompt. A
    /// group that combines our steer with another client's is treated as ours,
    /// so our text is never echoed back as a peer message.
    fn on_injected(
        &mut self,
        identity: TurnIdentity,
        event: ServerEvent,
        content: String,
        client_ids: &[String],
    ) -> Vec<RouteAction> {
        let ours = self.take_waiting(client_ids);
        for prompt_id in &ours {
            self.correlations
                .insert(*prompt_id, (turn_id_string(&identity), "steer"));
        }
        if let Some(own) = self
            .own_turn
            .as_mut()
            .filter(|own| own.identity == identity)
        {
            if ours.is_empty() {
                let request_id = own.request_id;
                self.deliver(request_id, event);
            } else {
                own.joined.extend(ours);
            }
            return Vec::new();
        }
        if self.peer_matches(&identity) {
            if ours.is_empty() {
                let peer = self.peer_turn.as_mut().expect("matched above");
                let index = peer.steers;
                peer.steers += 1;
                return vec![RouteAction::PeerSteer {
                    turn_id: turn_id_string(&identity),
                    index,
                    content,
                }];
            }
            if self.view_closed {
                // The view closed while the steer waited: the turn stays a
                // peer turn.
                for prompt_id in ours {
                    self.resolve_cancelled(prompt_id);
                }
                return Vec::new();
            }
            // Adopt the turn: from here on its output answers our prompt.
            let peer = self.peer_turn.take().expect("matched above");
            let turn_id = turn_id_string(&peer.identity);
            let mut members = ours.into_iter();
            let request_id = members.next().expect("not empty");
            self.own_turn = Some(OwnTurn {
                identity: peer.identity,
                request_id,
                joined: members.collect(),
                adopted: true,
                bracket_open: true,
            });
            return vec![RouteAction::PeerTurnAdopted { turn_id }];
        }
        if ours.is_empty() {
            crate::logging::debug(&format!(
                "acp: dropping a steer injected into a turn that is not open: {}",
                turn_id_string(&identity)
            ));
            return Vec::new();
        }
        if self.own_turn.is_some() || self.view_closed || !self.fence.admits(&identity) {
            for prompt_id in ours {
                self.resolve_cancelled(prompt_id);
            }
            return Vec::new();
        }
        // A turn this view never saw start took our steer; it still answers
        // the prompt, without a bracket.
        let mut members = ours.into_iter();
        let request_id = members.next().expect("not empty");
        self.own_turn = Some(OwnTurn {
            identity,
            request_id,
            joined: members.collect(),
            adopted: true,
            bracket_open: false,
        });
        Vec::new()
    }

    /// Hand an adopted turn's owed `_jcode/turn` terminal to its prompt, which
    /// writes it after the turn's remaining output.
    fn end_adopted_bracket(
        &mut self,
        request_id: u64,
        identity: TurnIdentity,
        status: TurnEndStatus,
        message: Option<String>,
    ) {
        let Some(own) = self
            .own_turn
            .as_mut()
            .filter(|own| own.identity == identity && own.bracket_open)
        else {
            return;
        };
        own.bracket_open = false;
        self.deliver(
            request_id,
            ServerEvent::TurnEnded {
                identity,
                status,
                message,
            },
        );
    }

    /// An unscoped event: a reply, session state, or (rarely) a notice the
    /// daemon does not scope.
    fn on_flat(&mut self, event: ServerEvent) -> Vec<RouteAction> {
        if let ServerEvent::SoftInterruptsCancelled {
            id,
            cancelled,
            not_queued,
        } = &event
        {
            if self.probes.remove(id) {
                return self.on_probe_reply(*id, cancelled, not_queued);
            }
            return Vec::new();
        }
        if let ServerEvent::SoftInterruptInjected { client_ids, .. } = &event {
            // `turn_start`: the daemon ran our steer as a new turn, announced
            // by its `turn_started`. Never part of a turn's stream.
            self.take_waiting(client_ids);
            return Vec::new();
        }
        if let ServerEvent::Error { id, message, .. } = &event
            && let Some(actions) = self.on_rejected(*id, message)
        {
            return actions;
        }
        let mut actions = Vec::new();
        if let ServerEvent::History { .. } = &event
            && !self.is_catalog_reply(&event)
        {
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

    /// A model-catalog reply reuses `History` but omits the live-session list
    /// and transcript, so it must not overwrite session metadata.
    pub(super) fn is_catalog_reply(&self, event: &ServerEvent) -> bool {
        reply_request_id(event)
            .and_then(|id| self.pending.get(&id))
            .is_some_and(|pending| pending.kind == PendingKind::Catalog)
    }

    /// Session metadata from a `History`. The first one is the attach
    /// snapshot: a peer turn announced before it whose prompt it already
    /// contains must not show that prompt again.
    fn on_history(&mut self, event: &ServerEvent) -> Vec<RouteAction> {
        let ServerEvent::History {
            session_id,
            all_sessions,
            messages,
            ..
        } = event
        else {
            return Vec::new();
        };
        self.session_id = session_id.clone();
        self.live_sessions = Some(all_sessions.clone());
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
        if let ServerEvent::History {
            session_id,
            all_sessions,
            ..
        } = &event
            && !self.is_catalog_reply(&event)
        {
            self.session_id = session_id.clone();
            self.live_sessions = Some(all_sessions.clone());
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
        let mut actions = Vec::new();
        if let Some(own) = self.own_turn.take() {
            actions.extend(Self::adopted_bracket_end(
                &own,
                Some("The session's runtime was replaced before the turn ended.".to_string()),
            ));
            for member in own.members() {
                self.deliver(
                    member,
                    ServerEvent::Error {
                        id: member,
                        message: "The session's runtime was replaced before the turn ended."
                            .to_string(),
                        retry_after_secs: None,
                    },
                );
            }
        }
        // The replaced runtime's queue went with it.
        let waiting: Vec<u64> = self.waiting.keys().copied().collect();
        self.waiting.clear();
        self.probes.clear();
        for prompt_id in waiting {
            self.deliver(
                prompt_id,
                ServerEvent::Error {
                    id: prompt_id,
                    message: "The session's runtime was replaced before the prompt ran."
                        .to_string(),
                    retry_after_secs: None,
                },
            );
        }
        actions.extend(self.close_peer_turn(
            TurnEndStatus::Cancelled,
            Some("The session's runtime was replaced before the turn ended.".to_string()),
        ));
        actions
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
            steers: 0,
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

/// End status of a turn whose scoped terminal arrived without `turn_ended`.
fn terminal_status(event: &ServerEvent) -> (TurnEndStatus, Option<String>) {
    match event {
        ServerEvent::Error { message, .. } => (TurnEndStatus::Failed, Some(message.clone())),
        _ => (TurnEndStatus::Completed, None),
    }
}

/// A turn's terminal addressed to prompt `request_id`: the daemon's carries
/// the id of the request that started the turn, which a steered or adopting
/// prompt does not share.
fn retarget_terminal(event: &ServerEvent, request_id: u64) -> ServerEvent {
    match event {
        ServerEvent::Error {
            message,
            retry_after_secs,
            ..
        } => ServerEvent::Error {
            id: request_id,
            message: message.clone(),
            retry_after_secs: *retry_after_secs,
        },
        _ => ServerEvent::Done { id: request_id },
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
            reasoning_effort,
            available_reasoning_efforts,
            error: None,
            ..
        } => Some(SessionStateChange::Model {
            model: model.clone(),
            provider_name: provider_name.clone(),
            reasoning_effort: reasoning_effort.clone(),
            available_reasoning_efforts: available_reasoning_efforts.clone(),
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
