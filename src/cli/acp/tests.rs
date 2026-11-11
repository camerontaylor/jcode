use super::*;

mod presentation;
mod reconnect;
mod requests;
mod session;

mod reexec;

#[test]
fn acp_turn_outcome_maps_terminal_limits_and_refusals() {
    use crate::protocol::TurnStopReason;
    let limit = |raw: Option<&str>, msg: &str| ServerEvent::TurnStopped {
        reason: TurnStopReason::LimitReached,
        message: msg.to_string(),
        provider_stop_reason: raw.map(str::to_string),
    };
    let mut o = TurnOutcome::default();
    o.observe(&ServerEvent::Ack { id: 7 }, 7);
    assert_eq!(o.stop_reason(false), "end_turn");
    o.observe(&limit(Some("max_tokens"), "truncated"), 7);
    assert_eq!(o.stop_reason(false), "max_tokens");
    // A continued model request clears an intermediate limit.
    o.observe(&ServerEvent::RetryRollback { attempt: 1, max: 3 }, 7);
    assert_eq!(o.stop_reason(false), "end_turn");
    o.observe(&limit(Some("max_turn_requests"), "limit"), 7);
    assert_eq!(o.stop_reason(false), "max_turn_requests");

    let mut o = TurnOutcome::default();
    o.observe(&ServerEvent::Ack { id: 7 }, 7);
    o.observe(
        &ServerEvent::ProviderGuardrail {
            stop_reason: Some("refusal".into()),
            message: "blocked".into(),
        },
        7,
    );
    assert_eq!(o.stop_reason(false), "refusal");
    // Cancellation wins over everything for the cancelled prompt.
    assert_eq!(o.stop_reason(true), "cancelled");

    // Non-refusal provider notices are not refusals.
    let mut o = TurnOutcome::default();
    o.observe(
        &ServerEvent::ProviderGuardrail {
            stop_reason: Some("end_turn".into()),
            message: "empty".into(),
        },
        7,
    );
    assert_eq!(o.stop_reason(false), "end_turn");
}

#[test]
fn acp_turn_outcome_ignores_stale_interrupt_before_ack() {
    let mut o = TurnOutcome::default();
    o.observe(&ServerEvent::Interrupted, 9);
    o.observe(&ServerEvent::Ack { id: 9 }, 9);
    assert_eq!(o.stop_reason(false), "end_turn");
    o.observe(&ServerEvent::Interrupted, 9);
    assert_eq!(o.stop_reason(false), "cancelled");
}

#[test]
fn routed_outcome_preserves_unacknowledged_withdrawal() {
    let mut outcome = TurnOutcome::routed();
    outcome.observe(&ServerEvent::Interrupted, 7);
    outcome.observe(&ServerEvent::Done { id: 7 }, 7);
    assert_eq!(outcome.stop_reason(false), "cancelled");
}
