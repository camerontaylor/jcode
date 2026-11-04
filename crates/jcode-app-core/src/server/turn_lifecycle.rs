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
/// `History.active_turn` populated until the turn ends.
pub(super) struct TurnLifecycle {
    turn_id: String,
    cancel_epoch: u64,
    registry: crate::turn_lifecycle_registry::ActiveTurnGuard,
}

impl TurnLifecycle {
    /// Emit `turn_started` and mark the turn active. Call while holding the
    /// agent lock, immediately before the turn's first stream event.
    pub(super) fn start(
        agent: &Agent,
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
        origin: TurnOrigin,
        user_content: Option<String>,
    ) -> Self {
        let turn_id = next_turn_id();
        let cancel_epoch = agent.graceful_shutdown_signal().epoch();
        let registry = crate::turn_lifecycle_registry::register_active_turn(
            agent.session_id(),
            ActiveTurn {
                turn_id: turn_id.clone(),
                origin: origin.clone(),
                user_content: user_content.clone(),
            },
        );
        let _ = event_tx.send(ServerEvent::TurnStarted {
            turn_id: turn_id.clone(),
            origin,
            user_content,
        });
        Self {
            turn_id,
            cancel_epoch,
            registry,
        }
    }

    /// Emit `turn_ended` before the turn's terminal `done`/`error` and before
    /// `turn_stopped`/`soft_interrupt_queue`.
    pub(super) fn finish(
        self,
        agent: &Agent,
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
        result: &anyhow::Result<()>,
    ) {
        let TurnLifecycle {
            turn_id,
            cancel_epoch,
            registry,
        } = self;
        // Clear the active turn first so a client attaching in this window
        // cannot open a turn that has already ended.
        drop(registry);
        let status = match result {
            // The turn loop breaks instead of failing when its cancel signal
            // fired, so an interrupted turn still reports `Ok`. The signal's
            // epoch is the only reliable record that a cancel landed during
            // this turn.
            Ok(()) if agent.graceful_shutdown_signal().epoch() != cancel_epoch => {
                TurnEndStatus::Cancelled
            }
            Ok(()) => TurnEndStatus::Completed,
            Err(_) => TurnEndStatus::Failed,
        };
        let message = result.as_ref().err().map(crate::util::format_error_chain);
        let _ = event_tx.send(ServerEvent::TurnEnded {
            turn_id,
            status,
            message,
        });
    }
}
