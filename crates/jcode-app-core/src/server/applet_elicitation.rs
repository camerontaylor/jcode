//! Applet answers share arbitration with structured form clients.
use super::client_actions::{NotifySessionContext, handle_notify_session};
use super::*;

/// A user pressed something in an agent applet: store its state, publish, then
/// resolve a waiting `applet` tool call or wake the agent like a notification.
pub(super) async fn handle_applet_action(
    id: u64,
    session_id: String,
    instance: String,
    action: jcode_applet_types::Action,
    state: serde_json::Value,
    source_key: Option<String>,
    ctx: NotifySessionContext<'_>,
) {
    if !crate::tool::applet_elicit::claim(
        &session_id,
        &instance,
        crate::tool::applet_elicit::Source::Desktop,
    ) {
        if let Err(error) = ctx.client_event_tx.send(ServerEvent::Done { id }) {
            crate::logging::debug(&format!("applet acknowledgement failed: {error}"));
        }
        return;
    }
    let stored = match crate::applets::set_state(&session_id, &instance, state) {
        Ok(stored) => stored,
        Err(error) => {
            crate::tool::applet_elicit::release_claim(&session_id, &instance);
            send(
                ctx.client_event_tx,
                ServerEvent::Error {
                    id,
                    message: error.to_string(),
                    retry_after_secs: None,
                },
            );
            return;
        }
    };
    let (snapshot, inst) = stored;
    crate::tool::applet::publish(&session_id, snapshot);
    let message = crate::tool::applet::format_action_message(
        &instance,
        &inst.document.title,
        &action,
        &inst.document.state,
        source_key.as_deref(),
    );
    if crate::tool::applet::deliver_to_waiter(&session_id, &instance, message.clone()) {
        send(ctx.client_event_tx, ServerEvent::Done { id });
        return;
    }
    handle_notify_session(id, session_id, message, ctx).await;
}

fn send(events: &mpsc::UnboundedSender<ServerEvent>, event: ServerEvent) {
    if let Err(error) = events.send(event) {
        crate::logging::debug(&format!("applet response delivery failed: {error}"));
    }
}
