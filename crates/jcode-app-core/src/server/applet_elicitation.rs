//! Desktop applet answers share arbitration with structured form clients.
use super::client_actions::NotifySessionContext;
use super::*;

/// A losing Desktop answer is acknowledged without applying its state.
pub(super) fn answered(
    id: u64,
    session: &str,
    instance: &str,
    ctx: &NotifySessionContext<'_>,
) -> bool {
    if crate::tool::applet_elicit::claim(
        session,
        instance,
        crate::tool::applet_elicit::Source::Desktop,
    ) {
        return false;
    }
    if let Err(error) = ctx.client_event_tx.send(ServerEvent::Done { id }) {
        crate::logging::debug(&format!("applet acknowledgement failed: {error}"));
    }
    true
}
