use super::*;

#[test]
fn cancel_after_turn_claim_is_preserved() -> anyhow::Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_peer_cancel_then_message";

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime for TURN_CLAIM_HOOK");
    rt.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(FanoutStreamProvider);
        let registry = Registry::new(Arc::clone(&provider)).await;
        let mut session =
            crate::session::Session::create_with_id(session_id.to_string(), None, None);
        session.model = Some("fanout-stream".to_string());
        let agent = Arc::new(Mutex::new(Agent::new_with_session(
            provider, registry, session, None,
        )));
        let (agent_stop_signal, soft_interrupt_queue) = {
            let guard = agent.lock().await;
            (
                guard.graceful_shutdown_signal(),
                guard.soft_interrupt_queue(),
            )
        };
        // This connection's control handle holds the agent's own signal.
        let control = SessionControlHandle::cancel_only(
            session_id,
            soft_interrupt_queue,
            agent_stop_signal.clone(),
        );

        // A turn another connection (or a server-initiated path) runs.
        let peer_turn =
            crate::turn_cancel_registry::register_active_turn(session_id, InterruptSignal::new());

        let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
        let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
        let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (swarm_event_tx, _) = broadcast::channel(8);
        let swarm = SwarmStatusRefs {
            members: &swarm_members,
            swarms_by_id: &swarms_by_id,
            event_history: &event_history,
            event_counter: &event_counter,
            event_tx: &swarm_event_tx,
        };
        let mut client_is_processing = false;
        let mut message_id = None;
        let mut processing_session_id = None;
        let mut task = None;

        cancel_processing_message_for_turn(
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut processing_session_id,
                task: &mut task,
            },
            &control,
            &client_event_tx,
            &swarm,
            Some(1),
            None,
            None,
            None,
        )
        .await;
        assert!(
            agent_stop_signal.is_set(),
            "test precondition: the stop of a peer turn arms the session signal"
        );

        // The peer turn ends; a new message follows at once, well inside the
        // 500 ms window before the deferred reset.
        drop(peer_turn);
        while client_event_rx.try_recv().is_ok() {}
        let (processing_done_tx, _processing_done_rx) = mpsc::unbounded_channel();
        let claimed_signal = agent_stop_signal.clone();
        super::client_processing::TURN_CLAIM_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                claimed_signal.fire();
            }));
        });
        start_processing_message(
            ProcessingMessage {
                id: 2,
                content: "after the stop".to_string(),
                images: Vec::new(),
                system_reminder: None,
                active_skill: None,
                server_failover: false,
                client_instance_id: None,
            },
            session_id,
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut processing_session_id,
                task: &mut task,
            },
            &agent,
            &client_event_tx,
            &processing_done_tx,
            Vec::new(),
            &swarm,
        )
        .await;

        assert!(
            agent_stop_signal.is_set(),
            "a cancellation after claim must survive preparation"
        );
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    });
    Ok(())
}
