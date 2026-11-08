use super::*;

/// Force replacement at the first await in cancellation dispatch. Holding the
/// fanout lock is a deterministic scheduling seam; no provider timing is used.
#[tokio::test]
async fn cancellation_dispatch_cannot_signal_a_successor() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let mut harness =
        TurnHarness::new("dispatch_replacement", 2, Arc::new(HangingStreamProvider)).await;
    let mut requester = harness.take_receiver(0);
    for mode in 0..3 {
        let guard = harness.agent.lock().await;
        let lease = (mode < 2).then(|| guard.claim_server_turn().unwrap());
        let identity = lease.as_ref().map(|lease| lease.identity().clone());
        let signal = guard.graceful_shutdown_signal();
        drop(guard);
        let control = SessionControlHandle::cancel_only(
            harness.session_id.clone(),
            Arc::new(std::sync::Mutex::new(Vec::new())),
            signal.clone(),
        );
        let mut processing = false;
        let mut message = None;
        let mut session = None;
        let mut task = None;
        let mut state = ProcessingState {
            client_is_processing: &mut processing,
            message_id: &mut message,
            session_id: &mut session,
            task: &mut task,
        };
        let swarm = harness.swarm();
        let blocked = harness.swarm_members.write().await;
        let cancel = super::client_processing::cancel_processing_message_for_turn(
            &mut state,
            &control,
            &harness.senders[0],
            &swarm,
            Some(2),
            None,
            Some(&harness.sessions),
            if mode == 0 { identity.as_ref() } else { None },
        );
        tokio::pin!(cancel);
        assert!(futures::poll!(cancel.as_mut()).is_pending());
        drop(lease);
        let successor = harness.agent.lock().await.claim_server_turn().unwrap();
        let _registered =
            crate::turn_cancel_registry::register_active_turn(&harness.session_id, signal.clone());
        signal.reset(); // preparation of the successor clears only earlier cancellation
        drop(blocked);
        cancel.await;
        assert!(
            !signal.is_set(),
            "dispatch must not signal the successor (mode={mode})"
        );
        if mode < 2 {
            let events: Vec<_> = std::iter::from_fn(|| requester.try_recv().ok()).collect();
            assert!(
                !events.iter().any(|event| scoped_payload(event).is_some_and(
                    |(_, event)| matches!(
                        event,
                        ServerEvent::TurnStopped { .. } | ServerEvent::Interrupted
                    )
                )),
                "a non-owning stale Stop must not publish stopped events: {events:?}"
            );
        }
        drop(successor);
    }
}

#[tokio::test]
async fn duplicate_producer_terminals_are_published_once() {
    let _lock = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(HangingStreamProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Agent::new(provider, registry);
    let (a_tx, mut a_rx) = mpsc::unbounded_channel();
    let (b_tx, mut b_rx) = mpsc::unbounded_channel();
    let members = Arc::new(RwLock::new(HashMap::from([(
        agent.session_id().to_string(),
        member(
            agent.session_id(),
            HashMap::from([("a".to_string(), a_tx), ("b".to_string(), b_tx)]),
        ),
    )])));
    let (fallback, mut fallback_rx) = mpsc::unbounded_channel();
    let lease = agent.claim_server_turn().unwrap();
    let identity = lease.identity().clone();
    let events = super::super::state::TurnEventChannel::new(
        agent.session_id().into(),
        Arc::clone(&members),
        fallback,
        &lease,
        33,
        crate::protocol::TurnOrigin::client(None, 33),
        None,
    );
    events
        .sender()
        .send(ServerEvent::TextDelta {
            text: "partial".into(),
        })
        .unwrap();
    events.sender().send(ServerEvent::Done { id: 33 }).unwrap();
    events.sender().send(ServerEvent::Done { id: 33 }).unwrap();
    events.finish().await;
    drop(lease);

    for rx in [&mut a_rx, &mut b_rx] {
        assert!(
            matches!(next_event(rx).await, ServerEvent::TurnStarted { identity: started, .. } if started == identity)
        );
        assert!(matches!(
            scoped_payload(&next_event(rx).await),
            Some((scope, ServerEvent::TextDelta { text })) if scope == &identity && text == "partial"
        ));
        let terminal = next_event(rx).await;
        assert!(matches!(
            &terminal,
            ServerEvent::TurnEvent {
                identity: ended,
                end: Some(crate::protocol::TurnEndStatus::Completed),
                ..
            } if ended == &identity
        ));
        assert!(matches!(
            scoped_payload(&terminal),
            Some((scope, ServerEvent::Done { id: 33 })) if scope == &identity
        ));
        assert!(rx.try_recv().is_err(), "exactly one terminal is published");
    }
    assert!(fallback_rx.try_recv().is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        while agent.claim_server_turn().is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("drain releases the runtime");
    assert!(a_rx.try_recv().is_err() && b_rx.try_recv().is_err());
}

#[tokio::test]
async fn failed_producer_terminal_is_published_once_after_output() {
    let _lock = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(HangingStreamProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Agent::new(provider, registry);
    let (a_tx, mut a_rx) = mpsc::unbounded_channel();
    let (b_tx, mut b_rx) = mpsc::unbounded_channel();
    let members = Arc::new(RwLock::new(HashMap::from([(
        agent.session_id().to_string(),
        member(
            agent.session_id(),
            HashMap::from([("a".to_string(), a_tx), ("b".to_string(), b_tx)]),
        ),
    )])));
    let (fallback, mut fallback_rx) = mpsc::unbounded_channel();
    let lease = agent.claim_server_turn().unwrap();
    let identity = lease.identity().clone();
    let events = super::super::state::TurnEventChannel::new(
        agent.session_id().into(),
        Arc::clone(&members),
        fallback,
        &lease,
        33,
        crate::protocol::TurnOrigin::client(None, 33),
        None,
    );
    events
        .sender()
        .send(ServerEvent::TextDelta {
            text: "partial".into(),
        })
        .unwrap();
    events
        .sender()
        .send(ServerEvent::Error {
            id: 33,
            message: "failure sentinel".into(),
            retry_after_secs: None,
        })
        .unwrap();
    events.sender().send(ServerEvent::Done { id: 33 }).unwrap();
    events.finish().await;
    drop(lease);

    for rx in [&mut a_rx, &mut b_rx] {
        assert!(
            matches!(next_event(rx).await, ServerEvent::TurnStarted { identity: started, .. } if started == identity)
        );
        assert!(matches!(
            scoped_payload(&next_event(rx).await),
            Some((scope, ServerEvent::TextDelta { text })) if scope == &identity && text == "partial"
        ));
        let terminal = next_event(rx).await;
        assert!(matches!(
            &terminal,
            ServerEvent::TurnEvent {
                identity: ended,
                end: Some(crate::protocol::TurnEndStatus::Failed),
                ..
            } if ended == &identity
        ));
        assert!(matches!(
            scoped_payload(&terminal),
            Some((scope, ServerEvent::Error { id: 33, message, .. })) if scope == &identity && message == "failure sentinel"
        ));
        assert!(rx.try_recv().is_err(), "exactly one terminal is published");
    }
    assert!(fallback_rx.try_recv().is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        while agent.claim_server_turn().is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("drain releases the runtime");
    assert!(a_rx.try_recv().is_err() && b_rx.try_recv().is_err());
}

/// Both producers use daemon dispatch. Withdrawal is decided by the queue's
/// insertion boundary, never by which observer remains attached.
#[tokio::test]
async fn two_steering_producers_withdraw_on_either_side_of_insertion() {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let (provider, gate) = GatedNumberedProvider::new();
    let mut harness = TurnHarness::new("steering_insertion_boundary", 3, provider).await;
    let control = {
        let agent = harness.agent.lock().await;
        SessionControlHandle::cancel_only(
            harness.session_id.clone(),
            agent.soft_interrupt_queue(),
            agent.graceful_shutdown_signal(),
        )
    };
    let mut observer = harness.take_receiver(2);
    let mut processing = false;
    let mut message = None;
    let mut session = None;
    let mut task = None;
    harness
        .start_prompt(
            0,
            60,
            "owner",
            &mut ProcessingState {
                client_is_processing: &mut processing,
                message_id: &mut message,
                session_id: &mut session,
                task: &mut task,
            },
        )
        .await;
    read_until(&mut observer, |event| {
        scoped_payload(event)
            .is_some_and(|(_, event)| matches!(event, ServerEvent::TextDelta { .. }))
    })
    .await;
    let (reply, mut replies) = mpsc::unbounded_channel();
    for (id, text) in [(61, "producer-a"), (62, "producer-b")] {
        queue_soft_interrupt(
            id,
            text.into(),
            vec![],
            false,
            Some(text.into()),
            &control,
            &reply,
        );
    }
    cancel_soft_interrupts_by_id(
        63,
        &harness.session_id,
        &["producer-a".into()],
        &control,
        &reply,
    );
    assert!(
        std::iter::from_fn(|| replies.try_recv().ok()).any(|event| matches!(event,
        ServerEvent::SoftInterruptsCancelled { cancelled, .. } if cancelled == vec!["producer-a"]))
    );
    drop(harness.take_receiver(0)); // first attachment leaves before insertion
    gate.add_permits(1);
    let injected = read_until(&mut observer, |event| {
        scoped_payload(event)
            .is_some_and(|(_, event)| matches!(event, ServerEvent::SoftInterruptInjected { .. }))
    })
    .await;
    assert!(!injected.iter().any(|event| scoped_payload(event).is_some_and(|(_, event)|
        matches!(event, ServerEvent::SoftInterruptInjected { content, .. } if content.contains("producer-a")))));
    cancel_soft_interrupts_by_id(
        64,
        &harness.session_id,
        &["producer-b".into()],
        &control,
        &reply,
    );
    assert!(std::iter::from_fn(|| replies.try_recv().ok()).any(|event| matches!(event,
        ServerEvent::SoftInterruptsCancelled { not_queued, .. } if not_queued == vec!["producer-b"])));
    drop(harness.take_receiver(1)); // second attachment leaves after insertion
    gate.add_permits(8);
    let ended = read_until(&mut observer, |event| is_scoped_done(event, 60)).await;
    assert!(ended.iter().any(|event| matches!(
        event,
        ServerEvent::TurnEvent {
            end: Some(crate::protocol::TurnEndStatus::Completed),
            ..
        }
    )));
    task.take().unwrap().await.unwrap();
    assert!(
        observer.try_recv().is_err(),
        "one terminal, after all inserted output"
    );
}

/// Reuse the dispatch lock seam, with independent publication so normal
/// completion can retire T while cancellation is paused before revalidation.
#[tokio::test]
async fn completed_turn_is_not_reported_interrupted_by_a_late_stop() {
    assert_completed_turn_survives_late_stop(false).await;
}

#[tokio::test]
async fn late_stop_preserves_task_for_completion_reconciliation() {
    assert_completed_turn_survives_late_stop(true).await;
}

async fn assert_completed_turn_survives_late_stop(check_handle_first: bool) {
    let _lock = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let mut harness =
        TurnHarness::new("completed_before_stop", 1, Arc::new(HangingStreamProvider)).await;
    let mut observer = harness.take_receiver(0);
    let guard = harness.agent.lock().await;
    let lease = guard.claim_server_turn().unwrap();
    let identity = lease.identity().clone();
    let signal = guard.graceful_shutdown_signal();
    drop(guard);
    let events = super::super::state::TurnEventChannel::new(
        harness.session_id.clone(),
        Arc::clone(&harness.swarm_members),
        harness.senders[0].clone(),
        &lease,
        77,
        crate::protocol::TurnOrigin::client(None, 77),
        Some("normal turn".into()),
    );
    let (complete, completed) = tokio::sync::oneshot::channel();
    let (retired, retirement) = tokio::sync::oneshot::channel();
    let (finish, finishing) = tokio::sync::oneshot::channel();
    let (exited, exit) = tokio::sync::oneshot::channel();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<ProcessingCompletion>();
    // Publication can complete before the producer's final bookkeeping exits.
    let mut task = Some(tokio::spawn(async move {
        completed.await.unwrap();
        events
            .sender()
            .send(ServerEvent::TextDelta {
                text: "normal output".into(),
            })
            .unwrap();
        events.sender().send(ServerEvent::Done { id: 77 }).unwrap();
        events.finish().await;
        drop(lease);
        retired.send(()).unwrap();
        finishing.await.unwrap();
        done_tx
            .send((77, Ok(()), None, tokio::task::id(), None))
            .unwrap();
        exited.send(()).unwrap();
    }));
    let producer_id = task.as_ref().unwrap().id();
    let control = SessionControlHandle::cancel_only(
        harness.session_id.clone(),
        Arc::new(std::sync::Mutex::new(Vec::new())),
        signal.clone(),
    );
    let (requester, mut replies) = mpsc::unbounded_channel();
    let mut processing = true;
    let mut message = Some(77);
    let mut session = Some(harness.session_id.clone());
    let mut state = ProcessingState {
        client_is_processing: &mut processing,
        message_id: &mut message,
        session_id: &mut session,
        task: &mut task,
    };
    // A separate outbound registry blocks dispatch without blocking T's drain.
    let dispatch_members = Arc::new(RwLock::new(HashMap::new()));
    let swarm = SwarmStatusRefs {
        members: &dispatch_members,
        ..harness.swarm()
    };
    let blocked = dispatch_members.write().await;
    let mut cancel = Box::pin(
        super::client_processing::cancel_processing_message_for_turn(
            &mut state,
            &control,
            &requester,
            &swarm,
            Some(78),
            None,
            Some(&harness.sessions),
            None,
        ),
    );
    assert!(futures::poll!(cancel.as_mut()).is_pending());
    complete.send(()).unwrap();
    let normal = read_until(&mut observer, |event| is_scoped_done(event, 77)).await;
    retirement.await.unwrap();
    assert!(normal.iter().any(|event| matches!(event, ServerEvent::TurnEvent {
        identity: ended, end: Some(crate::protocol::TurnEndStatus::Completed), .. } if ended == &identity)));
    assert_eq!(
        normal
            .iter()
            .filter(|event| is_scoped_done(event, 77))
            .count(),
        1
    );
    assert!(!normal.iter().any(|event| {
        scoped_payload(event).is_some_and(|(_, event)| {
            matches!(
                event,
                ServerEvent::TurnStopped { .. } | ServerEvent::Interrupted
            )
        })
    }));
    let epoch = signal.epoch();
    let successor = harness.agent.lock().await.claim_server_turn().unwrap();
    drop(blocked);
    cancel.as_mut().await;
    drop(cancel);
    finish.send(()).unwrap();
    exit.await.unwrap();
    if check_handle_first {
        assert_eq!(
            state.task.as_ref().map(|task| task.id()),
            Some(producer_id),
            "stale cancellation must retain the task for completion reconciliation"
        );
    }
    let replies: Vec<_> = std::iter::from_fn(|| replies.try_recv().ok()).collect();
    assert!(
        !replies.iter().any(
            |event| scoped_payload(event).is_some_and(|(scope, event)| scope == &identity
                && matches!(
                    event,
                    ServerEvent::TurnStopped { .. } | ServerEvent::Interrupted
                ))
        ),
        "a naturally completed turn must not be reported stopped: {replies:?}"
    );
    assert_eq!(
        signal.epoch(),
        epoch,
        "late Stop must not fire the successor signal"
    );
    assert!(!signal.is_set());
    assert_eq!(
        state.task.as_ref().map(|task| task.id()),
        Some(producer_id),
        "stale cancellation must retain the task for completion reconciliation"
    );
    let completion = done_rx.recv().await.unwrap();
    assert!(completion.1.is_ok());
    let connections = Arc::new(RwLock::new(HashMap::new()));
    reconcile_processing_completions(
        Some(completion),
        &mut done_rx,
        &mut state,
        "late-stop-owner",
        &connections,
        &swarm,
    )
    .await;
    assert!(state.task.is_none());
    assert!(
        !*state.client_is_processing,
        "the completion must be accepted"
    );
    assert!(state.message_id.is_none());
    assert!(state.session_id.is_none());
    drop(successor);
}
