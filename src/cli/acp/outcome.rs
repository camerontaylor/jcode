use super::*;

/// Tracks daemon events for one ACP prompt and derives its ACP stop reason.
///
/// Only the terminal outcome of the whole turn counts: a limit or refusal
/// followed by another model request (a successful continuation) is reset.
#[derive(Debug, Default)]
pub(super) struct TurnOutcome {
    acked: bool,
    interrupted: bool,
    terminal: Option<&'static str>,
}

impl TurnOutcome {
    /// The router has already isolated this prompt's events. It also emits
    /// interruptions for withdrawn prompts that were never acknowledged.
    pub(super) fn routed() -> Self {
        Self {
            acked: true,
            ..Self::default()
        }
    }

    pub(super) fn observe(&mut self, event: &ServerEvent, prompt_id: u64) {
        match event {
            ServerEvent::Ack { id } if *id == prompt_id => self.acked = true,
            // Interrupts read before our prompt was acknowledged are stale
            // leftovers from an earlier turn, not this prompt's outcome.
            ServerEvent::Interrupted if self.acked => self.interrupted = true,
            ServerEvent::KvCacheRequest { .. } | ServerEvent::RetryRollback { .. } => {
                self.terminal = None;
            }
            ServerEvent::TurnStopped {
                reason,
                message,
                provider_stop_reason,
            } => match reason {
                crate::protocol::TurnStopReason::LimitReached => {
                    let raw = provider_stop_reason.as_deref().unwrap_or_default();
                    let lower = message.to_ascii_lowercase();
                    self.terminal = Some(
                        if raw.contains("turn_requests")
                            || lower.contains("request limit")
                            || lower.contains("max turn")
                        {
                            "max_turn_requests"
                        } else {
                            "max_tokens"
                        },
                    );
                }
                crate::protocol::TurnStopReason::ProviderGuardrail => {
                    self.terminal = Some("refusal");
                }
                crate::protocol::TurnStopReason::Interrupted if self.acked => {
                    self.interrupted = true;
                }
                _ => {}
            },
            ServerEvent::ProviderGuardrail { stop_reason, .. } => {
                if stop_reason.as_deref().is_some_and(is_refusal_stop_reason) {
                    self.terminal = Some("refusal");
                }
            }
            _ => {}
        }
    }

    pub(super) fn stop_reason(&self, cancel_requested: bool) -> &'static str {
        if cancel_requested || self.interrupted {
            "cancelled"
        } else {
            self.terminal.unwrap_or("end_turn")
        }
    }
}

fn is_refusal_stop_reason(reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    [
        "refusal",
        "content_filter",
        "safety",
        "blocked",
        "prohibited",
        "guardrail",
    ]
    .iter()
    .any(|needle| reason.contains(needle))
}
