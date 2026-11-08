use super::*;

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

#[test]
fn a_completed_todo_write_asks_the_daemon_for_the_plan_snapshot() {
    let mut router = attached_router();
    let mut prompt = router.register(77, PendingKind::Prompt);
    router.route(turn_started(1, ours(77), Some("plan it")));
    let actions = router.route(scoped(
        1,
        ServerEvent::ToolDone {
            id: "call-todo".to_string(),
            name: "todo".to_string(),
            output: "[{\"name\":\"one\"}]".to_string(),
            error: None,
        },
    ));
    // The tool's own output is a rendered card; the plan comes from the
    // structured snapshot the daemon owns.
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, RouteAction::Send(Request::GetTodos { .. }))),
        "{actions:?}"
    );
    assert!(
        drain(&mut prompt)
            .iter()
            .any(|event| matches!(event, ServerEvent::ToolDone { .. }))
    );
}

#[test]
fn other_tool_completions_do_not_request_a_plan_snapshot() {
    let mut router = attached_router();
    let mut prompt = router.register(77, PendingKind::Prompt);
    router.route(turn_started(1, ours(77), Some("read it")));
    let actions = router.route(scoped(
        1,
        ServerEvent::ToolDone {
            id: "call-read".to_string(),
            name: "read".to_string(),
            output: "contents".to_string(),
            error: None,
        },
    ));
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, RouteAction::Send(Request::GetTodos { .. }))),
        "{actions:?}"
    );
    let _ = drain(&mut prompt);
}

#[test]
fn a_todo_snapshot_is_a_session_plan_action_whenever_it_arrives() {
    // The daemon writes the reply on the connection's event channel, which can
    // drain after the turn that produced it ended. The plan must still render,
    // so it is not scoped to a turn.
    let mut router = attached_router();
    let mut prompt = router.register(77, PendingKind::Prompt);
    router.route(turn_started(1, ours(77), Some("plan it")));
    router.route(scoped_terminal(1, TurnEndStatus::Completed));
    router.route(scoped(1, ServerEvent::Done { id: 77 }));
    assert!(!router.own_turn_running());
    let _ = drain(&mut prompt);

    let payload =
        json!({"todos": [{"content": "one", "status": "pending", "priority": "high"}]}).to_string();
    let actions = router.route(ServerEvent::Todos {
        session_id: "session-1".to_string(),
        payload: payload.clone(),
    });
    assert!(
        matches!(
            actions.as_slice(),
            [RouteAction::Plan { payload: routed }] if *routed == payload
        ),
        "{actions:?}"
    );
}

#[test]
fn a_todo_snapshot_during_a_turn_is_not_delivered_as_turn_output() {
    let mut router = attached_router();
    let mut prompt = router.register(77, PendingKind::Prompt);
    router.route(turn_started(1, ours(77), Some("plan it")));
    let payload =
        json!({"todos": [{"content": "one", "status": "pending", "priority": "high"}]}).to_string();
    let actions = router.route(ServerEvent::Todos {
        session_id: "session-1".to_string(),
        payload,
    });
    assert!(
        matches!(actions.as_slice(), [RouteAction::Plan { .. }]),
        "{actions:?}"
    );
    assert!(
        !drain(&mut prompt)
            .iter()
            .any(|event| matches!(event, ServerEvent::Todos { .. }))
    );
}

#[test]
fn closed_views_drop_todo_snapshots_and_refreshes() {
    for detach in [false, true] {
        let mut router = attached_router();
        if detach {
            router.detach_view();
        } else {
            router.close_view();
        }
        assert!(
            router
                .route(ServerEvent::Todos {
                    session_id: "session-1".into(),
                    payload: "[]".into(),
                })
                .is_empty()
        );
        assert!(
            router
                .route(scoped(
                    1,
                    ServerEvent::ToolDone {
                        id: "todo".into(),
                        name: "todo".into(),
                        output: "[]".into(),
                        error: None,
                    }
                ))
                .is_empty()
        );
    }
}

#[test]
fn late_prompt_registration_after_detach_or_close_is_rejected() {
    for detach in [false, true] {
        let mut router = attached_router();
        // Deterministic late-task interleaving: closure wins the admission lock.
        if detach {
            router.detach_view();
        } else {
            router.close_view();
        }
        assert!(router.register_open_prompt(99).is_none());
        assert!(!router.pending.contains_key(&99));
    }
}
