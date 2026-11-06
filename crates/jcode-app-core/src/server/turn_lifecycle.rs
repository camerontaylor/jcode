//! Daemon turn lifecycle events (`turn_started`/`turn_ended`).
//!
//! Every server path that streams a model turn to a session's ordered event
//! sender wraps the turn in one [`TurnLifecycle`] pair. The pair is emitted at
//! the *logical* turn level, after the agent lock is held, so turns that queue
//! on the agent mutex emit `TurnStarted` only when they actually start and
//! lifecycle pairs never nest within a session.
//!
//! It is deliberately not emitted inside
//! `process_locked_message_streaming_mpsc`: server failover retries that
//! primitive more than once for one logical turn.
//!
//! The pair is also closed when the turn's future is dropped instead of
//! finishing: a cancelled turn is aborted once its grace period expires
//! (`client_lifecycle::cancel_processing_message`), and a panicking turn
//! unwinds through its task. Both would otherwise leave every attached client
//! holding a turn that never ends, which is exactly what a peer-turn mirror
//! needs to keep its `_jcode/turn` bracket honest.

use crate::agent::Agent;
use crate::protocol::{ActiveTurn, ServerEvent, TurnEndStatus, TurnOrigin};
use jcode_agent_runtime::InterruptSignal;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

static NEXT_TURN_ID: AtomicU64 = AtomicU64::new(1);

/// Opaque, daemon-unique turn id.
fn next_turn_id() -> String {
    format!("turn-{}", NEXT_TURN_ID.fetch_add(1, Ordering::Relaxed))
}

/// The user-visible prompt of a turn, or `None` when there is nothing to show
/// (reminder-only continuations, empty wake turns).
pub(super) fn user_content(content: &str) -> Option<String> {
    let trimmed = content.trim();
    (!trimmed.is_empty()).then(|| content.to_string())
}

/// One logical turn's lifecycle registration. Keeps the session's
/// `History.active_turn` populated until the turn ends, and closes the pair on
/// drop when the turn never reported its own end.
pub(super) struct TurnLifecycle {
    turn_id: String,
    /// The agent's cancel signal, captured at start so the pair can be closed
    /// without taking the agent lock again: a queued model turn must not delay
    /// a connection-local turn's end event.
    cancel_signal: InterruptSignal,
    cancel_epoch: u64,
    /// `Some` while the turn is published as the session's active turn.
    registry: Option<crate::turn_lifecycle_registry::ActiveTurnGuard>,
    /// The same ordered sender the turn streams on, so the end event can still
    /// be sent when the turn is dropped mid-stream.
    event_tx: mpsc::UnboundedSender<ServerEvent>,
    closed: bool,
}

impl TurnLifecycle {
    /// Publish the turn as the session's active turn, then emit
    /// `turn_started`. Call while holding the agent lock, immediately before
    /// the turn's first stream event.
    pub(super) fn start(
        agent: &Agent,
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
        origin: TurnOrigin,
        user_content: Option<String>,
    ) -> Self {
        Self::start_with(agent, event_tx, origin, user_content, |announcement| {
            let _ = event_tx.send(announcement);
        })
    }

    /// [`start`](Self::start) with the announcement handed to `emit` instead of
    /// being sent here. The registration has already happened by the time
    /// `emit` runs, which is what makes "registered before announced" explicit
    /// and testable.
    pub(super) fn start_with(
        agent: &Agent,
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
        origin: TurnOrigin,
        user_content: Option<String>,
        emit: impl FnOnce(ServerEvent),
    ) -> Self {
        let turn_id = next_turn_id();
        let cancel_signal = agent.graceful_shutdown_signal();
        let cancel_epoch = cancel_signal.epoch();
        // Register *before* announcing. A client that attaches in this window
        // registers its event sender and then builds `History` from this
        // registry: if the announcement came first, that `History` would carry
        // `active_turn: None` and the attacher would drop every event of the
        // turn as belonging to no open turn.
        let registry = crate::turn_lifecycle_registry::register_active_turn(
            agent.session_id(),
            ActiveTurn {
                turn_id: turn_id.clone(),
                origin: origin.clone(),
                user_content: user_content.clone(),
            },
        );
        emit(ServerEvent::TurnStarted {
            turn_id: turn_id.clone(),
            origin,
            user_content,
        });
        Self {
            turn_id,
            cancel_signal,
            cancel_epoch,
            registry: Some(registry),
            event_tx: event_tx.clone(),
            closed: false,
        }
    }

    /// Like [`start`](Self::start) but without publishing the turn in
    /// `History.active_turn`: used for a turn that only exists on one
    /// connection (a `/subagent` tool launch), which is not the session's
    /// logical turn and must not hide one.
    pub(super) fn start_connection_local(
        agent: &Agent,
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
        origin: TurnOrigin,
        user_content: Option<String>,
    ) -> Self {
        let turn_id = next_turn_id();
        let cancel_signal = agent.graceful_shutdown_signal();
        let cancel_epoch = cancel_signal.epoch();
        let _ = event_tx.send(ServerEvent::TurnStarted {
            turn_id: turn_id.clone(),
            origin,
            user_content,
        });
        Self {
            turn_id,
            cancel_signal,
            cancel_epoch,
            registry: None,
            event_tx: event_tx.clone(),
            closed: false,
        }
    }

    /// Emit `turn_ended` before the turn's terminal `done`/`error` and before
    /// `turn_stopped`/`soft_interrupt_queue`.
    ///
    /// Takes no agent: the cancel signal captured at start answers the only
    /// question `finish` has about the agent, so a turn that does not hold the
    /// agent lock (a `/subagent` launch) never waits for one to report its end.
    pub(super) fn finish(mut self, result: &anyhow::Result<()>) {
        let status = match result {
            // The turn loop breaks instead of failing when its cancel signal
            // fired, so an interrupted turn still reports `Ok`. The signal's
            // epoch is the only reliable record that a cancel landed during
            // this turn.
            Ok(()) if self.cancel_signal.epoch() != self.cancel_epoch => TurnEndStatus::Cancelled,
            Ok(()) => TurnEndStatus::Completed,
            Err(_) => TurnEndStatus::Failed,
        };
        let message = result.as_ref().err().map(crate::util::format_error_chain);
        self.close(status, message);
    }

    /// Emit the end event once and clear the active-turn registration.
    fn close(&mut self, status: TurnEndStatus, message: Option<String>) {
        if self.closed {
            return;
        }
        self.closed = true;
        // Clear the active turn first so a client attaching in this window
        // cannot open a turn that has already ended.
        drop(self.registry.take());
        let _ = self.event_tx.send(ServerEvent::TurnEnded {
            turn_id: self.turn_id.clone(),
            status,
            message,
        });
    }
}

impl Drop for TurnLifecycle {
    /// Close the pair when the turn's future is dropped without `finish`: an
    /// aborted turn (its cancel grace period expired) or a panicking one.
    /// Without this, every attached client keeps a turn that never ends.
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        // A future dropped without `finish` is either an aborted turn or an
        // early return that skipped the pair. Neither can be told apart here, so
        // the message says what is true of both and the status reports the
        // common case: an aborted turn is `cancelled`, a panic is `failed`.
        let (status, message) = if std::thread::panicking() {
            (
                TurnEndStatus::Failed,
                Some("The turn panicked before it reported completion.".to_string()),
            )
        } else {
            (
                TurnEndStatus::Cancelled,
                Some("The turn ended before it reported completion.".to_string()),
            )
        };
        self.close(status, message);
    }
}
