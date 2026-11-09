use super::{tests::lock_telemetry_test_state, *};

#[test]
fn deferred_terminal_report_keeps_original_snapshot_and_clears_once() {
    let _guard = lock_telemetry_test_state();
    *SESSION_STATE.lock().unwrap() = None;
    begin_session("old-provider", "old-model");
    record_turn();
    let old_id = SESSION_STATE
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .session_id
        .clone();
    let report = prepare_session_end(
        "old-provider",
        "old-model",
        SessionEndReason::NormalExit,
        false,
        true,
    )
    .unwrap();
    assert!(SESSION_STATE.lock().unwrap().is_none());
    assert!(
        prepare_session_end(
            "old-provider",
            "old-model",
            SessionEndReason::NormalExit,
            false,
            true
        )
        .is_none()
    );
    begin_session("new-provider", "new-model");
    let new_id = SESSION_STATE
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .session_id
        .clone();
    TEST_EMITTED_PAYLOADS.lock().unwrap().clear();
    TEST_DELIVERY_MODES.lock().unwrap().clear();
    std::thread::spawn(move || report.emit()).join().unwrap();
    let payloads = TEST_EMITTED_PAYLOADS.lock().unwrap();
    let terminal: Vec<_> = payloads
        .iter()
        .filter(|payload| payload["event"] == "session_end")
        .collect();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0]["session_id"], old_id);
    assert_eq!(terminal[0]["provider_end"], "old-provider");
    assert_eq!(
        SESSION_STATE.lock().unwrap().as_ref().unwrap().session_id,
        new_id
    );
    assert!(
        TEST_DELIVERY_MODES
            .lock()
            .unwrap()
            .iter()
            .all(|mode| matches!(mode, DeliveryMode::Background))
    );
}
