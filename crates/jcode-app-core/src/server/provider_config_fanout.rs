//! Publish acknowledged configuration changes without changing request replies.
use super::{ServerEvent, SwarmMember};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{RwLock, mpsc};

/// The mutation may finish later while the agent is busy. Forward its result
/// when it actually completes, rather than sampling state after dispatch.
pub(super) fn config_reply_channel(
    members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    session_id: &str,
    connection_id: &str,
    requester: &mpsc::UnboundedSender<ServerEvent>,
) -> mpsc::UnboundedSender<ServerEvent> {
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    let members = Arc::clone(members);
    let session_id = session_id.to_owned();
    let connection_id = connection_id.to_owned();
    let requester = requester.clone();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            // Peer events use the broadcast id, never another connection's
            // request id (request counters are local to each connection).
            let mut peer = event.clone();
            let publish = match &mut peer {
                ServerEvent::ModelChanged {
                    id, error: None, ..
                }
                | ServerEvent::ReasoningEffortChanged {
                    id, error: None, ..
                } => {
                    *id = 0;
                    true
                }
                _ => false,
            };
            let _ = requester.send(event);
            if publish {
                super::state::fanout_peer_client_event(&members, &session_id, &connection_id, peer)
                    .await;
            }
        }
    });
    tx
}
