use super::*;
use crate::protocol::TurnStopReason;

const OURS: &str = "acp-test";
const PEER: &str = "acp-peer";
const GEN: &str = "gen-a";

/// A router on a daemon that advertised `turn_origin`.
fn router() -> RouterState {
    let mut router = RouterState::new(OURS, "session-1");
    router.set_turn_origin(true);
    router
}

/// A router on a daemon without `turn_origin`: events are read flat.
fn legacy_router() -> RouterState {
    RouterState::new(OURS, "session-1")
}

fn identity(turn_id: u64) -> TurnIdentity {
    identity_in(GEN, turn_id)
}

fn identity_in(generation: &str, turn_id: u64) -> TurnIdentity {
    TurnIdentity {
        runtime_generation: generation.to_string(),
        turn_id,
    }
}

fn tid(turn_id: u64) -> String {
    format!("{GEN}:{turn_id}")
}

fn turn_state(latest: u64, active: Option<(u64, Option<TurnOrigin>, Option<&str>)>) -> ServerEvent {
    turn_state_in(GEN, latest, active)
}

fn turn_state_in(
    generation: &str,
    latest: u64,
    active: Option<(u64, Option<TurnOrigin>, Option<&str>)>,
) -> ServerEvent {
    let (turn_id, active_origin, active_user_content) = match active {
        Some((turn_id, origin, content)) => (Some(turn_id), origin, content.map(str::to_string)),
        None => (None, None, None),
    };
    ServerEvent::TurnState {
        session_id: "session-1".to_string(),
        runtime_generation: generation.to_string(),
        turn_id,
        latest_turn_id: latest,
        active_origin,
        active_user_content,
    }
}

/// `History` with a rendered transcript of `(role, content)` messages.
fn history_event_with(messages: &[(&str, &str)]) -> ServerEvent {
    let messages: Vec<Value> = messages
        .iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect();
    serde_json::from_value(json!({
        "type": "history",
        "id": 1,
        "session_id": "session-1",
        "messages": messages,
        "all_sessions": ["session-1"],
    }))
    .expect("history decodes")
}

fn history_event() -> ServerEvent {
    history_event_with(&[])
}

/// A router that has completed an (idle) attach handshake.
fn attached_router() -> RouterState {
    let mut router = router();
    router.route(turn_state(0, None));
    router.route(history_event());
    router
}

fn turn_started(turn_id: u64, origin: TurnOrigin, content: Option<&str>) -> ServerEvent {
    ServerEvent::TurnStarted {
        identity: identity(turn_id),
        origin: Some(origin),
        user_content: content.map(str::to_string),
    }
}

fn scoped_terminal(turn_id: u64, status: TurnEndStatus) -> ServerEvent {
    ServerEvent::TurnEvent {
        end_message: None,
        identity: identity(turn_id),
        end: Some(status),
        event: Box::new(if status == TurnEndStatus::Failed {
            ServerEvent::Error {
                id: 0,
                message: "provider failed".into(),
                retry_after_secs: None,
            }
        } else {
            ServerEvent::Done { id: 0 }
        }),
    }
}

fn scoped(turn_id: u64, event: ServerEvent) -> ServerEvent {
    ServerEvent::TurnEvent {
        end_message: None,
        end: None,
        identity: identity(turn_id),
        event: Box::new(event),
    }
}

fn delta(text: &str) -> ServerEvent {
    ServerEvent::TextDelta {
        text: text.to_string(),
    }
}

fn interrupted_notice() -> ServerEvent {
    ServerEvent::TurnStopped {
        reason: TurnStopReason::Interrupted,
        message: "The turn was interrupted by a cancellation request.".to_string(),
        provider_stop_reason: None,
    }
}

fn ours(request_id: u64) -> TurnOrigin {
    TurnOrigin::client(Some(OURS.to_string()), request_id)
}

fn peer(request_id: u64) -> TurnOrigin {
    TurnOrigin::client(Some(PEER.to_string()), request_id)
}

fn drain_with_ends(
    rx: &mut RoutedReceiver,
) -> (Vec<ServerEvent>, Vec<(usize, TurnIdentity, TurnEndStatus)>) {
    let mut events = Vec::new();
    let mut ends = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            RoutedEvent::Event(event) => events.push(event),
            RoutedEvent::LocalTurnEnd {
                identity, status, ..
            } => ends.push((events.len(), identity, status)),
        }
    }
    (events, ends)
}

fn drain(rx: &mut RoutedReceiver) -> Vec<ServerEvent> {
    drain_with_ends(rx).0
}

fn texts(events: &[ServerEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::TextDelta { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn done_count(events: &[ServerEvent], id: u64) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, ServerEvent::Done { id: done } if *done == id))
        .count()
}

fn saw_done(events: &[ServerEvent], id: u64) -> bool {
    done_count(events, id) > 0
}

#[test]
fn own_turn_streams_to_the_prompt_waiter_only() {
    let mut router = attached_router();
    let mut prompt = router.register(77, PendingKind::Prompt);

    assert!(
        router
            .route(turn_started(1, ours(77), Some("what is the answer")))
            .is_empty(),
        "an own turn is bracketed by the prompt response, not a peer mirror"
    );
    assert!(router.route(scoped(1, delta("answer"))).is_empty());
    assert!(
        router
            .route(scoped_terminal(1, TurnEndStatus::Completed))
            .is_empty()
    );
    assert!(
        router
            .route(scoped(1, ServerEvent::Done { id: 77 }))
            .is_empty()
    );

    let events = drain(&mut prompt);
    assert_eq!(texts(&events), vec!["answer".to_string()]);
    assert_eq!(
        done_count(&events, 77),
        1,
        "the envelope is unwrapped before delivery: {events:?}"
    );
    assert!(!router.own_turn_running());

    // Nothing of a finished turn, scoped or not, is queued for the next prompt.
    assert!(router.route(scoped(1, delta("late"))).is_empty());
    assert!(router.route(delta("unscoped")).is_empty());
    assert!(drain(&mut prompt).is_empty());
}

#[test]
fn peer_turn_while_idle_is_mirrored_and_its_done_is_ignored() {
    let mut router = attached_router();
    let mut prompt = router.register(77, PendingKind::Prompt);

    let actions = router.route(turn_started(9, peer(5), Some("peer prompt")));
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::PeerTurnStarted { turn_id, user_content, .. }]
            if *turn_id == tid(9) && user_content.as_deref() == Some("peer prompt")
    ));

    let actions = router.route(scoped(9, delta("peer text")));
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::PeerTurnEvent(ServerEvent::TextDelta { text })] if text == "peer text"
    ));

    let actions = router.route(scoped_terminal(9, TurnEndStatus::Completed));
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::PeerTurnEnded {
            turn_id,
            status: TurnEndStatus::Completed,
            ..
        }] if *turn_id == tid(9)
    ));

    // The peer's terminal carries its own connection-local id and its turn's
    // identity; neither must reach this adapter's pending prompt.
    assert!(
        router
            .route(scoped(9, ServerEvent::Done { id: 5 }))
            .is_empty()
    );
    assert!(drain(&mut prompt).is_empty());
}

#[test]
fn peer_turn_after_an_own_turn_mirrors_without_touching_the_prompt() {
    let mut router = attached_router();
    let mut prompt = router.register(77, PendingKind::Prompt);

    router.route(turn_started(1, ours(77), Some("mine")));
    router.route(scoped(1, delta("own answer")));
    router.route(scoped_terminal(1, TurnEndStatus::Completed));
    router.route(scoped(1, ServerEvent::Done { id: 77 }));

    // The adapter has not unregistered the prompt yet when a peer turn starts.
    let actions = router.route(turn_started(2, peer(8), Some("theirs")));
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::PeerTurnStarted { .. }]
    ));
    let actions = router.route(scoped(2, delta("peer answer")));
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::PeerTurnEvent(_)]
    ));
    let actions = router.route(scoped_terminal(2, TurnEndStatus::Failed));
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::PeerTurnEnded {
            status: TurnEndStatus::Failed,
            ..
        }]
    ));
    router.route(scoped(2, ServerEvent::Done { id: 8 }));

    let events = drain(&mut prompt);
    assert_eq!(texts(&events), vec!["own answer".to_string()]);
    assert_eq!(done_count(&events, 77), 1);
}

/// The daemon does not queue a prompt behind a running turn: it rejects it
/// with an unscoped error. That error is the prompt's answer, and the peer
/// turn keeps mirroring untouched.
#[test]
fn own_prompt_during_a_peer_turn_gets_the_daemon_rejection() {
    let mut router = attached_router();
    router.route(turn_started(1, peer(4), Some("peer first")));
    let mut prompt = router.register(78, PendingKind::Prompt);

    assert_eq!(
        router.route(scoped(1, delta("peer output"))).len(),
        1,
        "peer output keeps mirroring while our prompt is in flight"
    );
    assert!(
        router.route(ServerEvent::Ack { id: 78 }).is_empty(),
        "the ack is a reply"
    );
    assert!(
        router
            .route(ServerEvent::Error {
                id: 78,
                message: "Already processing a message".to_string(),
                retry_after_secs: None,
            })
            .is_empty()
    );
    let events = drain(&mut prompt);
    assert!(texts(&events).is_empty(), "no peer output in our prompt");
    assert!(matches!(
        events.as_slice(),
        [ServerEvent::Ack { id: 78 }, ServerEvent::Error { id: 78, message, .. }]
            if message == "Already processing a message"
    ));
    assert!(!router.own_turn_running());
    assert_eq!(router.active_turn_identity(), Some(identity(1)));
}

#[test]
fn peer_done_with_a_colliding_id_does_not_complete_the_own_prompt() {
    let mut router = attached_router();
    let mut prompt = router.register(77, PendingKind::Prompt);

    // A peer turn whose connection-local request id happens to be ours.
    router.route(turn_started(1, peer(77), Some("theirs")));
    router.route(scoped(1, delta("peer answer")));
    router.route(scoped_terminal(1, TurnEndStatus::Completed));
    router.route(scoped(1, ServerEvent::Done { id: 77 }));
    assert!(
        drain(&mut prompt).is_empty(),
        "the peer's done must not finish our prompt"
    );

    // Our own turn and terminal for the same id still land.
    router.route(turn_started(2, ours(77), Some("mine")));
    router.route(scoped(2, delta("own answer")));
    router.route(scoped_terminal(2, TurnEndStatus::Completed));
    router.route(scoped(2, ServerEvent::Done { id: 77 }));
    let events = drain(&mut prompt);
    assert_eq!(texts(&events), vec!["own answer".to_string()]);
    assert_eq!(done_count(&events, 77), 1);
}

#[test]
fn undecodable_lines_are_skipped_and_never_end_the_session() {
    assert!(decode_daemon_line("").is_none());
    assert!(decode_daemon_line("not json at all").is_none());
    assert!(
        decode_daemon_line(r#"{"type":"event_from_a_newer_daemon"}"#).is_none(),
        "an unknown event type must be skipped, not fatal"
    );
    assert!(matches!(
        decode_daemon_line(r#"{"type":"text_delta","text":"hi"}"#),
        Some(ServerEvent::TextDelta { text }) if text == "hi"
    ));

    // A scripted line sequence with garbage in it still routes correctly.
    let mut router = attached_router();
    let mut prompt = router.register(77, PendingKind::Prompt);
    for line in [
        r#"{"type":"turn_started","identity":{"runtime_generation":"gen-a","turn_id":1},"origin":{"kind":"client","client_instance_id":"acp-test","request_id":77}}"#,
        "{ this is not json",
        r#"{"type":"turn_event","identity":{"runtime_generation":"gen-a","turn_id":1},"event":{"type":"text_delta","text":"answer"}}"#,
        r#"{"type":"turn_ended","identity":{"runtime_generation":"gen-a","turn_id":1},"status":"completed"}"#,
        r#"{"type":"turn_event","identity":{"runtime_generation":"gen-a","turn_id":1},"event":{"type":"done","id":77}}"#,
    ] {
        if let Some(event) = decode_daemon_line(line) {
            router.route(event);
        }
    }
    let events = drain(&mut prompt);
    assert_eq!(texts(&events), vec!["answer".to_string()]);
    assert!(saw_done(&events, 77));
}

#[test]
fn legacy_daemon_forwards_to_a_running_prompt_and_drains_idle_events() {
    let mut router = legacy_router();
    assert!(!router.supports_turn_origin());
    assert!(
        router.route(delta("idle peer output")).is_empty(),
        "idle events are drained on a legacy daemon"
    );

    let mut prompt = router.register(77, PendingKind::Prompt);
    router.route(delta("running output"));
    // A wrapped event is unwrapped for a flat reader.
    router.route(scoped(1, delta("wrapped output")));
    router.route(ServerEvent::Done { id: 77 });
    let events = drain(&mut prompt);
    assert_eq!(
        texts(&events),
        vec!["running output".to_string(), "wrapped output".to_string()]
    );
    assert!(saw_done(&events, 77));
    // The adapter unregisters the prompt when its turn completes.
    router.unregister(77);

    // Back to idle: nothing accumulates for the next prompt, and nothing is
    // mirrored or tracked as a turn.
    assert!(router.route(delta("after the turn")).is_empty());
    assert!(
        router
            .route(turn_started(2, peer(3), Some("peer")))
            .is_empty()
    );
    assert_eq!(router.active_turn_identity(), None);
    assert!(drain(&mut prompt).is_empty());
}

#[test]
fn mid_turn_attach_opens_the_running_turn_from_turn_state() {
    let mut router = router();
    let actions = router.route(turn_state(
        5,
        Some((5, Some(peer(3)), Some("attached prompt"))),
    ));
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::PeerTurnStarted { turn_id, user_content, origin, .. }]
            if *turn_id == tid(5)
                && user_content.as_deref() == Some("attached prompt")
                && origin.client_instance_id.as_deref() == Some(PEER)
    ));
    // The History that follows does not contain the prompt, so it stays.
    assert!(router.route(history_event()).is_empty());

    let actions = router.route(scoped(5, delta("mid-turn output")));
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::PeerTurnEvent(ServerEvent::TextDelta { text })] if text == "mid-turn output"
    ));
    // The live `turn_started` racing the snapshot does not open it twice.
    assert!(
        router
            .route(turn_started(5, peer(3), Some("attached prompt")))
            .is_empty()
    );

    let actions = router.route(scoped_terminal(5, TurnEndStatus::Cancelled));
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::PeerTurnEnded {
            status: TurnEndStatus::Cancelled,
            ..
        }]
    ));
}

#[test]
fn an_unannounced_active_turn_is_not_mirrored() {
    // A debug path can claim a turn id without publishing it: the snapshot then
    // reports an active turn with no origin, and no turn_ended will ever come.
    let mut router = router();
    assert!(
        router
            .route(turn_state(4, Some((4, None, None))))
            .is_empty()
    );
    assert_eq!(router.active_turn_identity(), None);
}

#[test]
fn session_state_from_peers_becomes_an_update_and_replies_still_route() {
    let mut router = attached_router();

    let model_changed =
        |id: u64, model: &str, provider_name: Option<&str>| ServerEvent::ModelChanged {
            id,
            model: model.to_string(),
            provider_name: provider_name.map(str::to_string),
            context_window: None,
            error: None,
            resolved_credential: None,
            provider_oauth_email: None,
            provider_oauth_account_label: None,
            available_reasoning_efforts: None,
            reasoning_effort: None,
        };
    let actions = router.route(model_changed(0, "peer-model", Some("peer-provider")));
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::SessionState(SessionStateChange::Model { model, .. })]
            if model == "peer-model"
    ));

    // A peer model change carries the new model's effort ladder and level.
    let mut with_efforts = model_changed(0, "peer-model", Some("peer-provider"));
    if let ServerEvent::ModelChanged {
        available_reasoning_efforts,
        reasoning_effort,
        ..
    } = &mut with_efforts
    {
        *available_reasoning_efforts = Some(vec!["low".into(), "high".into()]);
        *reasoning_effort = Some("low".into());
    }
    let actions = router.route(with_efforts);
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::SessionState(SessionStateChange::Model {
            model,
            reasoning_effort,
            available_reasoning_efforts,
            ..
        })] if model == "peer-model"
            && reasoning_effort.as_deref() == Some("low")
            && available_reasoning_efforts.as_ref().unwrap() == &["low", "high"]
    ));

    let actions = router.route(ServerEvent::SessionRenamed {
        session_id: "session-1".to_string(),
        title: Some("renamed".to_string()),
        display_title: "Renamed".to_string(),
    });
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::SessionState(SessionStateChange::Renamed { title })] if title == "Renamed"
    ));

    // A model change this adapter asked for is a reply, not session state.
    let mut reply = router.register(90, PendingKind::Reply);
    assert!(router.route(model_changed(90, "mine", None)).is_empty());
    assert!(matches!(
        drain(&mut reply).as_slice(),
        [ServerEvent::ModelChanged { id: 90, .. }]
    ));

    // The capability probe's answer is a reply too.
    let mut probe = router.register(91, PendingKind::Reply);
    router.route(ServerEvent::Pong {
        id: 91,
        native_ssh_protocol: None,
        capabilities: vec!["turn_origin".to_string()],
        user_connected_session_ids: None,
    });
    assert!(matches!(
        drain(&mut probe).as_slice(),
        [ServerEvent::Pong { id: 91, .. }]
    ));
}

/// A peer turn that never reported its end must be closed before the next
/// turn starts, or that turn's output is mirrored as the stale peer's.
#[test]
fn peer_turn_that_never_ended_is_closed_by_the_next_turn() {
    let mut router = attached_router();
    let actions = router.route(turn_started(1, peer(5), Some("peer prompt")));
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::PeerTurnStarted { .. }]
    ));
    assert!(matches!(
        router.route(scoped(1, delta("peer output"))).as_slice(),
        [RouteAction::PeerTurnEvent(_)]
    ));

    // No turn_ended ever arrives for turn 1. Our own prompt's turn starts.
    let mut prompt = router.register(78, PendingKind::Prompt);
    let actions = router.route(turn_started(2, ours(78), Some("mine")));
    assert!(
        matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnEnded {
                turn_id,
                status: TurnEndStatus::Cancelled,
                ..
            }] if *turn_id == tid(1)
        ),
        "the stale peer turn is closed when the next turn starts: {actions:?}"
    );

    // The own turn's output goes to the prompt, not the peer mirror, and the
    // dead peer turn cannot swallow our terminal.
    assert!(router.route(scoped(2, delta("own answer"))).is_empty());
    assert!(router.route(scoped(1, delta("stale peer"))).is_empty());
    router.route(scoped(2, ServerEvent::Done { id: 78 }));
    let events = drain(&mut prompt);
    assert_eq!(texts(&events), vec!["own answer".to_string()]);
    assert!(saw_done(&events, 78));
}

/// A peer's scoped terminal closes a peer turn whose `turn_ended` never
/// arrived, and cannot complete our prompt.
#[test]
fn peer_terminal_closes_a_peer_turn_that_never_ended() {
    let mut router = attached_router();
    let mut prompt = router.register(77, PendingKind::Prompt);

    router.route(turn_started(1, peer(9), Some("peer prompt")));
    let actions = router.route(scoped(1, ServerEvent::Done { id: 9 }));
    assert!(
        matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnEnded {
                turn_id,
                status: TurnEndStatus::Completed,
                ..
            }] if *turn_id == tid(1)
        ),
        "the peer's completion closes its turn: {actions:?}"
    );
    assert!(drain(&mut prompt).is_empty());
}

/// A peer turn's terminal is told apart from ours by its identity, so a late
/// one with our request id is dropped even while our own turn is running.
#[test]
fn a_late_peer_terminal_is_fenced_by_identity() {
    let mut router = attached_router();
    router.route(turn_started(1, peer(77), Some("peer prompt")));
    router.route(scoped_terminal(1, TurnEndStatus::Completed));

    let mut prompt = router.register(77, PendingKind::Prompt);
    router.route(turn_started(2, ours(77), Some("mine")));
    assert!(router.route(scoped(2, delta("own answer"))).is_empty());

    assert!(
        router
            .route(scoped(1, ServerEvent::Done { id: 77 }))
            .is_empty()
    );
    assert!(
        !saw_done(&drain(&mut prompt), 77),
        "the peer's late done must not complete our prompt"
    );

    router.route(scoped(2, ServerEvent::Done { id: 77 }));
    assert!(saw_done(&drain(&mut prompt), 77));
}

/// The cancel path as the daemon emits it. The canceller gets the scoped
/// interrupt notice, the turn's `scoped_terminal(cancelled)` and scoped terminal,
/// then the cancel path's own scoped `interrupted` + `done` for the same
/// request id. A peer gets the notice, the end and the turn's terminal (the
/// daemon fans it out to every attachment).
///
/// The daemon-side test of the same path is
/// `server::turn_event_channel_tests` (cancelled turns end for every
/// attachment).
#[test]
fn the_cancel_path_leaves_both_routers_consistent() {
    // The cancelling adapter's connection.
    let mut canceller = attached_router();
    let mut prompt = canceller.register(31, PendingKind::Prompt);
    canceller.route(turn_started(1, ours(31), Some("cancel me")));
    canceller.route(scoped(1, delta("streaming")));
    let actions = canceller.route(scoped(1, interrupted_notice()));
    assert!(actions.is_empty(), "own turn events stay on the prompt");
    canceller.route(scoped_terminal(1, TurnEndStatus::Cancelled));
    canceller.route(scoped(1, ServerEvent::Done { id: 31 }));
    // The cancel path's own notice and terminal, for a turn already closed.
    canceller.route(scoped(1, ServerEvent::Interrupted));
    canceller.route(scoped(1, ServerEvent::Done { id: 31 }));

    let events = drain(&mut prompt);
    assert_eq!(texts(&events), vec!["streaming".to_string()]);
    let interrupted = events
        .iter()
        .position(|event| matches!(event, ServerEvent::Interrupted))
        .expect("a cancelled own turn reports the interruption to its prompt");
    let done = events
        .iter()
        .position(|event| matches!(event, ServerEvent::Done { id: 31 }))
        .expect("the prompt completes");
    assert!(
        interrupted < done,
        "stop reason before completion: {events:?}"
    );
    assert_eq!(
        done_count(&events, 31),
        1,
        "the duplicate terminal of a closed turn is dropped: {events:?}"
    );
    assert!(!canceller.own_turn_running(), "the turn is closed");

    // A peer adapter attached to the same session.
    let mut peer_router = attached_router();
    let mut peer_prompt = peer_router.register(90, PendingKind::Prompt);
    peer_router.route(turn_started(1, peer(31), Some("cancel me")));
    peer_router.route(scoped(1, delta("streaming")));
    let actions = peer_router.route(scoped(1, interrupted_notice()));
    assert!(
        matches!(actions.as_slice(), [RouteAction::PeerTurnEvent(_)]),
        "the interrupt notice is mirrored inside the peer turn: {actions:?}"
    );
    let actions = peer_router.route(scoped_terminal(1, TurnEndStatus::Cancelled));
    assert!(
        matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnEnded {
                status: TurnEndStatus::Cancelled,
                ..
            }]
        ),
        "the end closes the peer turn exactly once: {actions:?}"
    );
    assert!(
        peer_router
            .route(scoped(1, ServerEvent::Done { id: 31 }))
            .is_empty()
    );
    assert!(
        drain(&mut peer_prompt).is_empty(),
        "nothing lands on a prompt that did not own the turn"
    );
}

/// This adapter stops a peer's turn (Paseo's stop during a TUI turn): it gets
/// the scoped notice and its own `interrupted`, both inside the mirrored
/// turn, then the peer turn's cancelled end.
#[test]
fn cancelling_a_peer_turn_keeps_it_mirrored_until_it_ends() {
    let mut router = attached_router();
    router.route(turn_started(4, peer(2), Some("tui prompt")));
    assert_eq!(router.active_turn_identity(), Some(identity(4)));
    assert!(matches!(
        router.route(ServerEvent::Ack { id: 500 }).as_slice(),
        []
    ));
    assert!(matches!(
        router.route(scoped(4, interrupted_notice())).as_slice(),
        [RouteAction::PeerTurnEvent(_)]
    ));
    assert!(matches!(
        router.route(scoped(4, ServerEvent::Interrupted)).as_slice(),
        [RouteAction::PeerTurnEvent(ServerEvent::Interrupted)]
    ));
    assert!(matches!(
        router
            .route(scoped_terminal(4, TurnEndStatus::Cancelled))
            .as_slice(),
        [RouteAction::PeerTurnEnded {
            status: TurnEndStatus::Cancelled,
            ..
        }]
    ));
    assert!(
        router
            .route(scoped(4, ServerEvent::Done { id: 2 }))
            .is_empty()
    );
    assert_eq!(router.active_turn_identity(), None);
}

/// A cancelled peer turn's terminal is fenced, so it cannot swallow a later
/// terminal that reuses its connection-local id.
#[test]
fn an_aborted_peer_turn_does_not_swallow_a_later_terminal_with_its_id() {
    let mut router = attached_router();
    router.route(turn_started(1, peer(77), Some("peer prompt")));
    let actions = router.route(scoped_terminal(1, TurnEndStatus::Cancelled));
    assert!(
        matches!(actions.as_slice(), [RouteAction::PeerTurnEnded { .. }]),
        "the cancelled peer turn closes: {actions:?}"
    );

    let mut prompt = router.register(77, PendingKind::Prompt);
    router.route(turn_started(2, ours(77), Some("mine")));
    router.route(scoped(2, delta("own answer")));
    router.route(scoped(2, ServerEvent::Done { id: 77 }));
    let events = drain(&mut prompt);
    assert_eq!(texts(&events), vec!["own answer".to_string()]);
    assert!(saw_done(&events, 77));
}

/// Stale events are fenced like the TUI fences them: ids at or below the
/// completed watermark, and anything from a retired runtime generation. Turn
/// ids may have gaps.
#[test]
fn stale_turns_and_retired_generations_are_fenced() {
    let mut router = router();
    router.route(turn_state(5, None));
    router.route(history_event());

    assert!(
        router.route(turn_started(3, peer(1), None)).is_empty(),
        "a turn at or below the watermark is stale"
    );
    assert!(
        matches!(
            router.route(turn_started(8, peer(1), None)).as_slice(),
            [RouteAction::PeerTurnStarted { turn_id, .. }] if *turn_id == tid(8)
        ),
        "a gap in turn ids is fine"
    );

    // The runtime is replaced: its open turn can never end here.
    let actions = router.route(turn_state_in("gen-b", 0, None));
    assert!(
        matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnEnded {
                status: TurnEndStatus::Cancelled,
                ..
            }]
        ),
        "the old runtime's turn is closed: {actions:?}"
    );
    assert!(
        router
            .route(ServerEvent::TurnStarted {
                identity: identity_in(GEN, 9),
                origin: Some(peer(1)),
                user_content: None,
            })
            .is_empty(),
        "a retired generation never comes back"
    );
    assert!(router.route(turn_state(9, None)).is_empty());
    assert!(matches!(
        router
            .route(ServerEvent::TurnStarted {
                identity: identity_in("gen-b", 1),
                origin: Some(peer(1)),
                user_content: None,
            })
            .as_slice(),
        [RouteAction::PeerTurnStarted { turn_id, .. }] if turn_id == "gen-b:1"
    ));
}

/// Closing the connection fails its waiters, refuses new ones, and ends a
/// peer turn that is still open.
#[test]
fn close_releases_waiters_and_ends_an_open_peer_turn() {
    let mut router = attached_router();
    let mut waiter = router.register(70, PendingKind::Reply);
    router.route(turn_started(1, peer(5), Some("peer prompt")));

    let actions = router.close();
    assert!(
        matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnEnded {
                turn_id,
                status: TurnEndStatus::Cancelled,
                ..
            }] if *turn_id == tid(1)
        ),
        "closing ends the open peer turn: {actions:?}"
    );
    assert!(
        matches!(
            waiter.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ),
        "waiters are released when the connection closes"
    );

    let mut late = router.register(71, PendingKind::Reply);
    assert!(
        matches!(
            late.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ),
        "a waiter registered after close fails immediately"
    );
}

/// A mid-turn attach gets the running turn's prompt in the snapshot itself,
/// so the announcement held behind the replay must not show it again.
#[test]
fn a_mid_turn_attach_does_not_re_announce_the_prompt_in_history() {
    let mut router = router();
    let actions = router.route(turn_state(
        5,
        Some((5, Some(peer(3)), Some("the running prompt"))),
    ));
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::PeerTurnStarted { .. }]
    ));
    let actions = router.route(history_event_with(&[
        ("user", "earlier prompt"),
        ("assistant", "earlier answer"),
        ("user", "the running prompt"),
    ]));
    assert!(
        matches!(
            actions.as_slice(),
            [RouteAction::PeerPromptReplayed { turn_id }] if *turn_id == tid(5)
        ),
        "the in-flight prompt is already replayed: {actions:?}"
    );
}

/// A peer turn announced live after the attach is new to this connection, so
/// its prompt is shown even when a later `History` happens to contain it.
#[test]
fn a_live_peer_turn_always_announces_its_prompt() {
    let mut router = attached_router();
    let actions = router.route(turn_started(1, peer(5), Some("peer prompt")));
    assert!(
        matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnStarted {
                prompt_already_replayed: false,
                ..
            }]
        ),
        "a live peer turn announces its prompt: {actions:?}"
    );
    // A catalog refresh during the turn carries the prompt too; nothing has
    // been replayed for it, so nothing is suppressed.
    router.route(turn_state(1, Some((1, Some(peer(5)), Some("peer prompt")))));
    assert!(
        router
            .route(history_event_with(&[("user", "peer prompt")]))
            .is_empty()
    );
}

/// The in-flight prompt may not be the very last replayed message (a tool
/// loop persists results after it), so the check looks at the tail.
#[test]
fn the_prompt_match_looks_past_trailing_tool_rounds() {
    let mut router = router();
    router.route(turn_state(
        5,
        Some((5, Some(peer(3)), Some("the running prompt"))),
    ));
    let actions = router.route(history_event_with(&[
        ("user", "the running prompt"),
        ("assistant", "working on it"),
        ("user", "tool result"),
    ]));
    assert!(
        matches!(actions.as_slice(), [RouteAction::PeerPromptReplayed { .. }]),
        "the prompt is found behind the trailing tool round: {actions:?}"
    );
}

/// A snapshot that reports our own running turn as active (a catalog reply
/// during our prompt) must not be mirrored as a peer turn.
#[test]
fn turn_state_for_our_own_turn_is_not_mirrored() {
    let mut router = attached_router();
    let mut prompt = router.register(77, PendingKind::Prompt);
    router.route(turn_started(1, ours(77), Some("mine")));

    let actions = router.route(turn_state(1, Some((1, Some(ours(77)), Some("mine")))));
    assert!(
        actions.is_empty(),
        "our own active turn is not a peer turn: {actions:?}"
    );
    router.route(scoped(1, delta("still ours")));
    assert_eq!(texts(&drain(&mut prompt)), vec!["still ours".to_string()]);
}

/// Every scoped event belongs to an open turn or nobody, and nested lifecycle
/// events cannot bypass that check.
#[test]
fn scoped_events_for_a_turn_that_is_not_open_are_dropped() {
    let mut router = attached_router();
    let mut prompt = router.register(77, PendingKind::Prompt);
    assert!(router.route(scoped(4, delta("unknown turn"))).is_empty());
    router.route(turn_started(5, ours(77), None));
    assert!(
        router
            .route(scoped(5, turn_started(6, peer(1), Some("smuggled"))))
            .is_empty()
    );
    router.route(scoped(5, scoped(5, delta("nested scope"))));
    assert_eq!(
        texts(&drain(&mut prompt)),
        vec!["nested scope".to_string()],
        "a doubly wrapped event is unwrapped, a nested start is not"
    );
    assert_eq!(router.own_turn_identity(), Some(identity(5)));
}

#[test]
fn catalog_history_preserves_live_sessions_and_attach_state() {
    for mut router in [attached_router(), {
        let mut legacy = legacy_router();
        legacy.route(history_event());
        legacy
    }] {
        let mut rx = router.register(2, PendingKind::Catalog);
        let mut event = history_event();
        if let ServerEvent::History {
            id,
            session_id,
            all_sessions,
            ..
        } = &mut event
        {
            *id = 2;
            *session_id = "catalog-session".to_string();
            all_sessions.clear();
        }
        router.route(event);
        assert_eq!(router.session_id(), "session-1");
        assert_eq!(
            router.live_sessions(),
            Some(["session-1".to_string()].as_slice())
        );
        assert!(matches!(
            drain(&mut rx).as_slice(),
            [ServerEvent::History { id: 2, .. }]
        ));
        // A real full-history snapshot still may clear the live list.
        let mut event = history_event();
        if let ServerEvent::History { all_sessions, .. } = &mut event {
            all_sessions.clear();
        }
        router.route(event);
        assert_eq!(router.live_sessions(), Some([].as_slice()));
    }
}

// --- Steering: prompts sent as soft interrupts ------------------------------

/// A router on a daemon that steers prompts (`turn_origin` and
/// `soft_interrupt_ids`), after an idle attach.
fn steering_router() -> RouterState {
    let mut router = attached_router();
    router.set_soft_interrupt_ids(true);
    router
}

fn cid(prompt_id: u64) -> String {
    format!("{OURS}/p{prompt_id}")
}

/// Register and track prompt `prompt_id` the way `run_prompt` does.
fn steer(router: &mut RouterState, prompt_id: u64) -> RoutedReceiver {
    let rx = router.register(prompt_id, PendingKind::Prompt);
    router.track_prompt(
        prompt_id,
        PromptDelivery::Steer,
        format!("steer {prompt_id}"),
        Vec::new(),
    );
    rx
}

fn injected(content: &str, client_ids: &[String]) -> ServerEvent {
    ServerEvent::SoftInterruptInjected {
        content: content.to_string(),
        display_role: None,
        point: "D".to_string(),
        tools_skipped: None,
        client_ids: client_ids.to_vec(),
    }
}

/// The `CancelSoftInterrupts` among `actions`: `(request id, client ids)`.
fn withdrawal(actions: &[RouteAction]) -> Option<(u64, Vec<String>)> {
    actions.iter().find_map(|action| match action {
        RouteAction::Send(Request::CancelSoftInterrupts {
            id,
            client_ids: Some(client_ids),
        }) => Some((*id, client_ids.clone())),
        _ => None,
    })
}

/// Request ids of the soft interrupts among `actions`.
fn resent(actions: &[RouteAction]) -> Vec<u64> {
    actions
        .iter()
        .filter_map(|action| match action {
            RouteAction::Send(Request::SoftInterrupt { id, .. }) => Some(*id),
            _ => None,
        })
        .collect()
}

fn wakes(actions: &[RouteAction]) -> Vec<SteerWake> {
    actions
        .iter()
        .filter_map(|action| match action {
            RouteAction::Wake { token, .. } => Some(*token),
            _ => None,
        })
        .collect()
}

fn cancelled_reply(id: u64, cancelled: &[String], not_queued: &[String]) -> ServerEvent {
    ServerEvent::SoftInterruptsCancelled {
        id,
        cancelled: cancelled.to_vec(),
        not_queued: not_queued.to_vec(),
    }
}

fn answered_cancelled(events: &[ServerEvent], prompt_id: u64) -> bool {
    events
        .iter()
        .any(|event| matches!(event, ServerEvent::Interrupted))
        && saw_done(events, prompt_id)
}

#[test]
fn steering_needs_both_capabilities_and_legacy_prompts_are_untracked() {
    let mut legacy = legacy_router();
    legacy.set_soft_interrupt_ids(true);
    assert!(!legacy.steering(), "no turn_origin, no steering");

    let mut router = attached_router();
    assert!(!router.steering(), "no soft_interrupt_ids, no steering");
    let _prompt = router.register(77, PendingKind::Prompt);
    router.track_prompt(77, PromptDelivery::Message, "hi".into(), Vec::new());
    assert!(router.steer_request(77).is_none());
    assert!(router.prompt_sent(77).is_empty());
    // A rejection on a non-steering daemon still reaches the prompt.
    router.route(ServerEvent::Error {
        id: 77,
        message: "Already processing a message".to_string(),
        retry_after_secs: None,
    });
}

#[test]
fn an_idle_steer_becomes_our_own_turn() {
    let mut router = steering_router();
    assert!(router.steering());
    let mut prompt = steer(&mut router, 77);

    let Some(Request::SoftInterrupt {
        id,
        content,
        urgent,
        client_id,
        ..
    }) = router.steer_request(77)
    else {
        panic!("a steering prompt is a soft interrupt");
    };
    assert_eq!((id, content.as_str(), urgent), (77, "steer 77", false));
    assert_eq!(client_id.as_deref(), Some(cid(77).as_str()));
    // Sent while nothing runs: a timer checks it is not stranded.
    assert_eq!(
        wakes(&router.prompt_sent(77)),
        vec![SteerWake {
            prompt_id: 77,
            kind: WakeKind::Probe
        }]
    );

    // The daemon runs it as a turn of our own (origin request id = the
    // soft interrupt's id) and confirms the pickup unscoped.
    assert!(
        router
            .route(turn_started(1, ours(77), Some("steer 77")))
            .is_empty()
    );
    let mut pickup = injected("steer 77", &[cid(77)]);
    if let ServerEvent::SoftInterruptInjected { point, .. } = &mut pickup {
        *point = "turn_start".to_string();
    }
    assert!(router.route(pickup).is_empty());
    assert!(router.route(scoped(1, delta("answer"))).is_empty());
    router.route(scoped_terminal(1, TurnEndStatus::Completed));
    assert!(
        router
            .route(scoped(1, ServerEvent::Done { id: 77 }))
            .is_empty(),
        "no steer is left to recover"
    );

    let events = drain(&mut prompt);
    assert_eq!(texts(&events), vec!["answer".to_string()]);
    assert_eq!(done_count(&events, 77), 1);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ServerEvent::SoftInterruptInjected { .. })),
        "our own pickup is never shown as a steer: {events:?}"
    );
    assert!(
        router
            .wake(SteerWake {
                prompt_id: 77,
                kind: WakeKind::Probe
            })
            .is_empty(),
        "the probe timer finds nothing to do once a turn took the prompt"
    );
}

#[test]
fn a_steer_into_our_running_turn_resolves_with_it() {
    let mut router = steering_router();
    let mut first = steer(&mut router, 77);
    router.route(turn_started(1, ours(77), Some("steer 77")));
    router.route(scoped(1, delta("first part")));

    // A second prompt from the same view while the turn runs.
    let mut second = steer(&mut router, 78);
    assert!(
        router.prompt_sent(78).is_empty(),
        "a turn is running, so its end checks the steer instead of a timer"
    );
    assert!(
        router
            .route(scoped(1, injected("steer 78", &[cid(78)])))
            .is_empty()
    );
    router.route(scoped(1, delta("second part")));
    router.route(scoped_terminal(1, TurnEndStatus::Completed));
    let actions = router.route(scoped(1, ServerEvent::Done { id: 77 }));
    assert!(
        withdrawal(&actions).is_none(),
        "an injected steer is not probed: {actions:?}"
    );

    let first = drain(&mut first);
    assert_eq!(
        texts(&first),
        vec!["first part".to_string(), "second part".to_string()],
        "the turn's whole stream answers the prompt that started it"
    );
    assert_eq!(done_count(&first, 77), 1);
    let second = drain(&mut second);
    assert!(texts(&second).is_empty());
    assert_eq!(
        done_count(&second, 78),
        1,
        "the steered prompt resolves with the turn, addressed to its own id: {second:?}"
    );
}

#[test]
fn our_steer_into_a_peer_turn_adopts_it() {
    for (status, terminal) in [
        (TurnEndStatus::Completed, ServerEvent::Done { id: 5 }),
        (TurnEndStatus::Cancelled, ServerEvent::Done { id: 5 }),
        (
            TurnEndStatus::Failed,
            ServerEvent::Error {
                id: 5,
                message: "provider failed".to_string(),
                retry_after_secs: None,
            },
        ),
    ] {
        let mut router = steering_router();
        let mut all_actions = router.route(turn_started(9, peer(5), Some("peer prompt")));
        let before = router.route(scoped(9, delta("peer before")));
        assert!(matches!(
            before.as_slice(),
            [RouteAction::PeerTurnEvent(ServerEvent::TextDelta { text })] if text == "peer before"
        ));

        let mut prompt = steer(&mut router, 77);
        assert!(router.prompt_sent(77).is_empty());
        assert!(
            drain(&mut prompt).is_empty(),
            "peer output before the injection stays with the mirror"
        );

        let actions = router.route(scoped(9, injected("steer 77", &[cid(77)])));
        assert!(
            matches!(
                actions.as_slice(),
                [RouteAction::PeerTurnAdopted { turn_id }] if *turn_id == tid(9)
            ),
            "our injection is not echoed as a peer message: {actions:?}"
        );
        assert_eq!(router.active_turn_identity(), Some(identity(9)));

        // From here the turn answers our prompt.
        assert!(router.route(scoped(9, delta("after steer"))).is_empty());
        all_actions.extend(router.route(scoped_terminal(9, status)));
        all_actions.extend(router.route(scoped(9, terminal.clone())));

        assert_eq!(
            all_actions
                .iter()
                .filter(|action| matches!(action, RouteAction::PeerTurnStarted { .. }))
                .count(),
            1,
            "one started bracket for the turn: {all_actions:?}"
        );
        assert!(
            !all_actions
                .iter()
                .any(|action| matches!(action, RouteAction::PeerTurnEnded { .. })),
            "the bracket is closed by the prompt, after its output: {all_actions:?}"
        );

        let (events, ends) = drain_with_ends(&mut prompt);
        assert_eq!(texts(&events), vec!["after steer".to_string()]);
        assert_eq!(ends.len(), 1, "exactly one adopted bracket end");
        let (marker_at, ended, ended_status) = &ends[0];
        assert_eq!(*ended, identity(9));
        assert_eq!(*ended_status, status);
        let text_at = events
            .iter()
            .position(|event| matches!(event, ServerEvent::TextDelta { .. }))
            .unwrap();
        assert!(text_at < *marker_at, "output precedes the bracket end");
        let terminal_at = events
            .iter()
            .position(|event| matches!(event, ServerEvent::Done { .. } | ServerEvent::Error { .. }))
            .unwrap();
        assert!(
            *marker_at <= terminal_at,
            "bracket precedes prompt settlement"
        );
        match status {
            TurnEndStatus::Completed => {
                assert_eq!(done_count(&events, 77), 1, "{events:?}");
                assert!(!events.iter().any(|e| matches!(e, ServerEvent::Interrupted)));
            }
            TurnEndStatus::Cancelled => assert!(answered_cancelled(&events, 77), "{events:?}"),
            TurnEndStatus::Failed => assert!(
                events.iter().any(|event| matches!(
                    event,
                    ServerEvent::Error { id: 77, message, .. } if message == "provider failed"
                )),
                "a failed turn fails the prompt, addressed to its id: {events:?}"
            ),
        }
        assert!(router.active_turn_identity().is_none());
    }
}

#[test]
fn peer_steers_are_mirrored_as_user_messages() {
    let mut router = steering_router();
    router.route(turn_started(9, peer(5), Some("peer prompt")));
    let first = router.route(scoped(9, injected("peer steer", &[])));
    assert!(matches!(
        first.as_slice(),
        [RouteAction::PeerSteer { turn_id, index: 0, content }]
            if *turn_id == tid(9) && content == "peer steer"
    ));
    let second = router.route(scoped(9, injected("sdk steer", &["sdk-1".to_string()])));
    assert!(matches!(
        second.as_slice(),
        [RouteAction::PeerSteer { index: 1, .. }]
    ));
    router.route(scoped_terminal(9, TurnEndStatus::Completed));

    // A peer steering our own turn is shown by the prompt, in stream order.
    let mut prompt = steer(&mut router, 77);
    router.route(turn_started(10, ours(77), Some("steer 77")));
    assert!(
        router
            .route(scoped(10, injected("tui steer", &[])))
            .is_empty()
    );
    let events = drain(&mut prompt);
    assert!(
        events.iter().any(|event| matches!(
            event,
            ServerEvent::SoftInterruptInjected { content, .. } if content == "tui steer"
        )),
        "{events:?}"
    );
}

#[test]
fn a_stranded_steer_is_recovered_with_cancel_by_id() {
    let mut router = steering_router();
    router.route(turn_started(9, peer(5), Some("peer prompt")));
    let mut prompt = steer(&mut router, 77);

    // The peer turn ends without injecting our steer.
    let actions = router.route(scoped_terminal(9, TurnEndStatus::Completed));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, RouteAction::PeerTurnEnded { .. }))
    );
    let (probe_id, client_ids) = withdrawal(&actions).expect("the stranded steer is probed");
    assert_eq!(client_ids, vec![cid(77)]);
    assert!(
        withdrawal(&router.route(scoped(9, ServerEvent::Done { id: 5 }))).is_none(),
        "one probe per stranded steer"
    );

    // Still queued: the daemon removed it, so it is sent again.
    let actions = router.route(cancelled_reply(probe_id, &[cid(77)], &[]));
    assert_eq!(resent(&actions), vec![77]);
    assert_eq!(
        wakes(&actions),
        vec![SteerWake {
            prompt_id: 77,
            kind: WakeKind::Probe
        }],
        "resent with no turn running, it is checked again later"
    );
    assert!(drain(&mut prompt).is_empty(), "the prompt is still waiting");

    // This time the session is idle and it starts our turn.
    router.route(turn_started(10, ours(77), Some("steer 77")));
    router.route(scoped(10, delta("finally")));
    router.route(scoped_terminal(10, TurnEndStatus::Completed));
    router.route(scoped(10, ServerEvent::Done { id: 77 }));
    let events = drain(&mut prompt);
    assert_eq!(texts(&events), vec!["finally".to_string()]);
    assert_eq!(done_count(&events, 77), 1);
}

#[test]
fn a_steer_reported_not_queued_waits_for_its_turn() {
    let mut router = steering_router();
    router.route(turn_started(9, peer(5), Some("peer prompt")));
    let mut prompt = steer(&mut router, 77);
    let actions = router.route(scoped_terminal(9, TurnEndStatus::Completed));
    let (probe_id, _) = withdrawal(&actions).unwrap();

    // A turn took it (here: the daemon started ours as the peer turn ended),
    // so nothing is resent; the turn's events follow.
    let actions = router.route(cancelled_reply(probe_id, &[], &[cid(77)]));
    assert!(resent(&actions).is_empty(), "never sent twice: {actions:?}");
    let confirm = wakes(&actions);
    assert_eq!(
        confirm,
        vec![SteerWake {
            prompt_id: 77,
            kind: WakeKind::Confirm
        }]
    );
    router.route(turn_started(10, ours(77), Some("steer 77")));
    assert!(
        router.wake(confirm[0]).is_empty(),
        "the turn arrived before the grace period ran out"
    );
    router.route(scoped(10, delta("ran")));
    router.route(scoped_terminal(10, TurnEndStatus::Completed));
    router.route(scoped(10, ServerEvent::Done { id: 77 }));
    let events = drain(&mut prompt);
    assert_eq!(texts(&events), vec!["ran".to_string()]);
    assert_eq!(done_count(&events, 77), 1);

    // A steer that left the queue and never ran (another client cleared the
    // queue) ends as cancelled once the grace period is over.
    let mut lost = steer(&mut router, 78);
    router.route(turn_started(11, peer(6), None));
    let (probe_id, _) =
        withdrawal(&router.route(scoped_terminal(11, TurnEndStatus::Completed))).unwrap();
    let confirm = wakes(&router.route(cancelled_reply(probe_id, &[], &[cid(78)])));
    assert!(router.wake(confirm[0]).is_empty());
    assert!(answered_cancelled(&drain(&mut lost), 78));
}

#[test]
fn a_steer_left_queued_while_no_turn_is_visible_is_probed_on_a_timer() {
    let mut router = steering_router();
    let mut prompt = steer(&mut router, 77);
    let wake = wakes(&router.prompt_sent(77));
    // The daemon queued it behind a turn that was finalizing; no turn end
    // will report that, so the timer asks.
    let actions = router.wake(wake[0]);
    let (probe_id, client_ids) = withdrawal(&actions).expect("probed");
    assert_eq!(client_ids, vec![cid(77)]);
    assert_eq!(
        resent(&router.route(cancelled_reply(probe_id, &[cid(77)], &[]))),
        vec![77]
    );
    assert!(drain(&mut prompt).is_empty());
}

#[test]
fn stop_withdraws_a_waiting_steer() {
    let mut router = steering_router();
    router.route(turn_started(9, peer(5), Some("peer prompt")));
    let mut prompt = steer(&mut router, 77);

    let actions = router.stop();
    let (probe_id, client_ids) = withdrawal(&actions).expect("withdrawn by id");
    assert_eq!(client_ids, vec![cid(77)]);
    assert_eq!(
        router.active_turn_identity(),
        Some(identity(9)),
        "the caller still cancels the running turn, scoped to it"
    );

    let actions = router.route(cancelled_reply(probe_id, &[cid(77)], &[]));
    assert!(
        resent(&actions).is_empty(),
        "a withdrawn steer is not resent"
    );
    assert!(answered_cancelled(&drain(&mut prompt), 77));

    // The peer turn still ends in the mirror, and nothing is probed after it.
    let actions = router.route(scoped_terminal(9, TurnEndStatus::Cancelled));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, RouteAction::PeerTurnEnded { .. }))
    );
    assert!(withdrawal(&actions).is_none());
}

#[test]
fn close_withdraws_a_waiting_steer_without_cancelling_any_turn() {
    let mut router = steering_router();
    router.route(turn_started(9, peer(5), Some("peer prompt")));
    let mut prompt = steer(&mut router, 77);

    let actions = router.close_view();
    let (probe_id, _) = withdrawal(&actions).expect("withdrawn by id");
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, RouteAction::Send(Request::Cancel { .. }))),
        "close never cancels another client's turn: {actions:?}"
    );
    router.route(cancelled_reply(probe_id, &[cid(77)], &[]));
    assert!(answered_cancelled(&drain(&mut prompt), 77));

    // A steer injected after the close (the withdrawal lost the race) does not
    // adopt the turn; the prompt answers as cancelled.
    let mut late = steer(&mut router, 78);
    let actions = router.route(scoped(9, injected("steer 78", &[cid(78)])));
    assert!(actions.is_empty(), "{actions:?}");
    assert!(answered_cancelled(&drain(&mut late), 78));
    assert_eq!(router.active_turn_identity(), None);
}

#[test]
fn close_cancels_only_a_turn_this_view_started() {
    // Our own turn: cancelled, scoped.
    let mut router = steering_router();
    let _prompt = steer(&mut router, 77);
    router.route(turn_started(1, ours(77), None));
    let actions = router.close_view();
    assert!(
        actions.iter().any(|action| matches!(
            action,
            RouteAction::Send(Request::Cancel { turn: Some(turn), .. }) if *turn == identity(1)
        )),
        "{actions:?}"
    );

    // An adopted peer turn: released, not cancelled.
    let mut router = steering_router();
    router.route(turn_started(9, peer(5), Some("peer prompt")));
    let mut prompt = steer(&mut router, 77);
    router.route(scoped(9, injected("steer 77", &[cid(77)])));
    assert_eq!(router.own_turn_identity(), Some(identity(9)));
    let actions = router.close_view();
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, RouteAction::Send(Request::Cancel { .. }))),
        "an adopted turn belongs to the client that started it: {actions:?}"
    );
    let (events, ends) = drain_with_ends(&mut prompt);
    assert!(
        ends.iter()
            .any(|(_, _, status)| *status == TurnEndStatus::Cancelled),
        "the adopted bracket closes for this view"
    );
    assert!(answered_cancelled(&events, 77));
    // Its remaining events are not routed anywhere.
    assert!(router.route(scoped(9, delta("still running"))).is_empty());
    assert!(
        router
            .route(scoped_terminal(9, TurnEndStatus::Completed))
            .is_empty()
    );
}

#[test]
fn a_steer_rejected_as_busy_is_retried() {
    let mut router = steering_router();
    let mut prompt = steer(&mut router, 77);
    let busy = ServerEvent::Error {
        id: 77,
        message: "Already processing a message".to_string(),
        retry_after_secs: None,
    };
    let actions = router.route(busy.clone());
    assert_eq!(
        wakes(&actions),
        vec![SteerWake {
            prompt_id: 77,
            kind: WakeKind::Retry
        }]
    );
    assert!(
        drain(&mut prompt).is_empty(),
        "the rejection is not the prompt's answer"
    );
    let actions = router.wake(SteerWake {
        prompt_id: 77,
        kind: WakeKind::Retry,
    });
    assert_eq!(resent(&actions), vec![77], "resent after the backoff");

    // Rejected again; a turn ending first resends it at once.
    router.route(busy);
    router.route(turn_started(9, peer(5), None));
    let actions = router.route(scoped_terminal(9, TurnEndStatus::Completed));
    assert_eq!(resent(&actions), vec![77]);
    assert!(
        router
            .wake(SteerWake {
                prompt_id: 77,
                kind: WakeKind::Retry
            })
            .is_empty(),
        "the stale backoff timer does not send it twice"
    );

    // A prompt sent as a `Message` (it carries a skill) that hits a busy
    // session steers into the running turn instead.
    let _skill = router.register(80, PendingKind::Prompt);
    router.track_prompt(80, PromptDelivery::Message, "with skill".into(), Vec::new());
    let actions = router.route(ServerEvent::Error {
        id: 80,
        message: "Already processing a message".to_string(),
        retry_after_secs: None,
    });
    assert_eq!(resent(&actions), vec![80]);

    // Any other error still answers the prompt.
    let mut other = steer(&mut router, 81);
    router.route(ServerEvent::Error {
        id: 81,
        message: "Skill 'x' is not installed on the server".to_string(),
        retry_after_secs: None,
    });
    assert!(
        drain(&mut other)
            .iter()
            .any(|event| matches!(event, ServerEvent::Error { id: 81, .. }))
    );
}

#[test]
fn eof_closes_an_adopted_turn_bracket() {
    let mut router = steering_router();
    router.route(turn_started(9, peer(5), Some("peer prompt")));
    let _prompt = steer(&mut router, 77);
    router.route(scoped(9, injected("steer 77", &[cid(77)])));
    let actions = router.close();
    assert!(
        matches!(
            actions.as_slice(),
            [RouteAction::PeerTurnEnded { turn_id, status: TurnEndStatus::Cancelled, .. }]
                if *turn_id == tid(9)
        ),
        "{actions:?}"
    );
}

#[test]
fn correlation_survives_terminal_until_request_release() {
    let mut r = steering_router();
    let _origin = steer(&mut r, 10);
    r.route(turn_started(1, ours(10), None));
    let _joined = steer(&mut r, 11);
    r.route(scoped(1, injected("steer", &[cid(11)])));
    r.route(scoped_terminal(1, TurnEndStatus::Completed));
    r.route(scoped(1, ServerEvent::Done { id: 10 }));
    assert_eq!(r.prompt_correlation(10), Some((tid(1), "origin")));
    assert_eq!(r.prompt_correlation(11), Some((tid(1), "steer")));
    r.unregister(10);
    assert_eq!(r.prompt_correlation(10), None);
}

#[test]
fn observer_close_ends_local_bracket_without_cancel() {
    let mut r = attached_router();
    r.route(turn_started(1, peer(10), None));
    let actions = r.close_view();
    assert!(matches!(
        actions.as_slice(),
        [RouteAction::PeerTurnEnded {
            status: TurnEndStatus::Cancelled,
            ..
        }]
    ));
    assert!(r.close_view().is_empty());
}

#[test]
fn detach_releases_owned_turn_without_cancel() {
    let mut r = steering_router();
    let mut prompt = steer(&mut r, 10);
    r.route(turn_started(1, ours(10), None));
    assert!(r.detach_view().is_empty());
    assert!(answered_cancelled(&drain(&mut prompt), 10));
    assert_eq!(r.prompt_correlation(10), Some((tid(1), "origin")));
    assert!(r.detach_view().is_empty());
}

#[test]
fn detach_observer_closes_bracket_before_release() {
    let mut r = steering_router();
    r.route(turn_started(1, peer(10), None));
    assert!(matches!(
        r.detach_view().as_slice(),
        [RouteAction::PeerTurnEnded {
            status: TurnEndStatus::Cancelled,
            ..
        }]
    ));
    assert!(r.detach_view().is_empty());
}
