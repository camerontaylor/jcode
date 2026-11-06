//! Process-global record of the turn currently running in each session.
//!
//! `turn_started`/`turn_ended` are emitted on the session's ordered event
//! stream, so a client that attaches mid-turn would otherwise never see the
//! opening event of the turn it is watching. The daemon therefore also reports
//! the running turn in `History.active_turn`, read from this registry.
//!
//! The registry is keyed by session id rather than held on the agent because
//! `History` is built by code paths that deliberately avoid locking the agent
//! (a busy agent must not block a history reply).

use crate::protocol::ActiveTurn;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

static ACTIVE_TURNS: LazyLock<Mutex<HashMap<String, ActiveTurn>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// RAII registration for one running turn. Dropping the guard clears the
/// session's entry, so a turn that ends by any path (error, panic, cancel)
/// cannot leave a stale `active_turn` behind.
pub struct ActiveTurnGuard {
    session_id: String,
    turn_id: String,
}

/// Record `turn` as the running turn of `session_id`.
pub fn register_active_turn(session_id: &str, turn: ActiveTurn) -> ActiveTurnGuard {
    let turn_id = turn.turn_id.clone();
    match ACTIVE_TURNS.lock() {
        Ok(mut map) => {
            map.insert(session_id.to_string(), turn);
        }
        Err(_) => crate::logging::warn(&format!(
            "TURN_LIFECYCLE_REGISTRY_LOCK_FAILED session={session_id}"
        )),
    }
    ActiveTurnGuard {
        session_id: session_id.to_string(),
        turn_id,
    }
}

/// The turn running in `session_id`, if any.
pub fn active_turn(session_id: &str) -> Option<ActiveTurn> {
    ACTIVE_TURNS
        .lock()
        .ok()
        .and_then(|map| map.get(session_id).cloned())
}

impl Drop for ActiveTurnGuard {
    fn drop(&mut self) {
        match ACTIVE_TURNS.lock() {
            // Only clear our own turn: a newer turn may already have replaced
            // the entry, and erasing that would hide a running turn.
            Ok(mut map) => {
                if map
                    .get(&self.session_id)
                    .is_some_and(|turn| turn.turn_id == self.turn_id)
                {
                    map.remove(&self.session_id);
                }
            }
            Err(_) => crate::logging::warn(&format!(
                "TURN_LIFECYCLE_REGISTRY_LOCK_FAILED session={} turn={}",
                self.session_id, self.turn_id
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{TurnOrigin, TurnOriginKind};

    fn turn(id: &str) -> ActiveTurn {
        ActiveTurn {
            turn_id: id.to_string(),
            origin: TurnOrigin::server(TurnOriginKind::Background),
            user_content: None,
        }
    }

    #[test]
    fn registration_is_visible_until_the_guard_drops() {
        let guard = register_active_turn("session-registry-a", turn("t1"));
        assert_eq!(
            active_turn("session-registry-a").map(|turn| turn.turn_id),
            Some("t1".to_string())
        );
        assert_eq!(active_turn("session-registry-other"), None);
        drop(guard);
        assert_eq!(active_turn("session-registry-a"), None);
    }

    #[test]
    fn a_newer_turn_survives_the_older_guards_drop() {
        let older = register_active_turn("session-registry-b", turn("t1"));
        let newer = register_active_turn("session-registry-b", turn("t2"));
        drop(older);
        assert_eq!(
            active_turn("session-registry-b").map(|turn| turn.turn_id),
            Some("t2".to_string())
        );
        drop(newer);
        assert_eq!(active_turn("session-registry-b"), None);
    }
}
