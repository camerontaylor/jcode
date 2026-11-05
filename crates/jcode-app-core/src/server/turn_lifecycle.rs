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
    cancel_epoch: u64,
    /// `Some` while the turn is published as the session's active turn.
    registry: Option<crate::turn_lifecycle_registry::ActiveTurnGuard>,
    /// The same ordered sender the turn streams on, so the end event can still
    /// be sent when the turn is dropped mid-stream.
    event_tx: mpsc::UnboundedSender<ServerEvent>,
    closed: bool,
}

impl TurnLifecycle {
    /// Emit `turn_started` and publish the turn as the session's active turn.
    /// Call while holding the agent lock, immediately before the turn's first
    /// stream event.
    pub(super) fn start(
        agent: &Agent,
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
        origin: TurnOrigin,
        user_content: Option<String>,
    ) -> Self {
        let mut lifecycle = Self::emit_start(event_tx, agent, origin.clone(), user_content.clone());
        lifecycle.registry = Some(crate::turn_lifecycle_registry::register_active_turn(
            agent.session_id(),
            ActiveTurn {
                turn_id: lifecycle.turn_id.clone(),
                origin,
                user_content,
            },
        ));
        lifecycle
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
        Self::emit_start(event_tx, agent, origin, user_content)
    }

    fn emit_start(
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
        agent: &Agent,
        origin: TurnOrigin,
        user_content: Option<String>,
    ) -> Self {
        let turn_id = next_turn_id();
        let cancel_epoch = agent.graceful_shutdown_signal().epoch();
        let _ = event_tx.send(ServerEvent::TurnStarted {
            turn_id: turn_id.clone(),
            origin,
            user_content,
        });
        Self {
            turn_id,
            cancel_epoch,
            registry: None,
            event_tx: event_tx.clone(),
            closed: false,
        }
    }

    /// Emit `turn_ended` before the turn's terminal `done`/`error` and before
    /// `turn_stopped`/`soft_interrupt_queue`.
    pub(super) fn finish(mut self, agent: &Agent, result: &anyhow::Result<()>) {
        let status = match result {
            // The turn loop breaks instead of failing when its cancel signal
            // fired, so an interrupted turn still reports `Ok`. The signal's
            // epoch is the only reliable record that a cancel landed during
            // this turn.
            Ok(()) if agent.graceful_shutdown_signal().epoch() != self.cancel_epoch => {
                TurnEndStatus::Cancelled
            }
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
        let (status, message) = if std::thread::panicking() {
            (
                TurnEndStatus::Failed,
                Some("The turn panicked before it reported completion.".to_string()),
            )
        } else {
            (
                TurnEndStatus::Cancelled,
                Some("The turn was aborted before it reported completion.".to_string()),
            )
        };
        self.close(status, message);
    }
}
