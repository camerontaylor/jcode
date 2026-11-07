use super::*;

#[test]
fn scoped_terminal_roundtrips_without_changing_legacy_done_shape() {
    let event = ServerEvent::TurnEvent {
        identity: crate::TurnIdentity {
            runtime_generation: "runtime-uuid".into(),
            turn_id: 7,
        },
        event: Box::new(ServerEvent::Done { id: 42 }),
    };
    let wire = encode_event(&event);
    let decoded: ServerEvent = serde_json::from_str(&wire).unwrap();
    let ServerEvent::TurnEvent { identity, event } = decoded else {
        panic!("missing scope");
    };
    assert_eq!(identity.runtime_generation, "runtime-uuid");
    assert_eq!(identity.turn_id, 7);
    assert!(matches!(*event, ServerEvent::Done { id: 42 }));
    let legacy: ServerEvent = serde_json::from_str(r#"{"type":"done","id":42}"#).unwrap();
    assert!(matches!(legacy, ServerEvent::Done { id: 42 }));
    assert_eq!(
        serde_json::to_value(&legacy).unwrap(),
        serde_json::json!({"type":"done","id":42})
    );
}

#[test]
fn old_subscribe_defaults_to_legacy_transport() {
    let request: Request = serde_json::from_str(r#"{"type":"subscribe","id":1}"#).unwrap();
    assert!(matches!(
        request,
        Request::Subscribe {
            supports_turn_identity: false,
            ..
        }
    ));
    let request: Request =
        serde_json::from_str(r#"{"type":"subscribe","id":1,"supports_turn_identity":true}"#)
            .unwrap();
    assert!(matches!(
        request,
        Request::Subscribe {
            supports_turn_identity: true,
            ..
        }
    ));
}

#[test]
fn bootstrap_roundtrip_carries_active_identity_and_idle_watermark() {
    for turn_id in [None, Some(7)] {
        let event = ServerEvent::TurnState {
            session_id: "session".into(),
            runtime_generation: "runtime".into(),
            turn_id,
            latest_turn_id: 7,
            active_origin: None,
            active_user_content: None,
        };
        let decoded: ServerEvent = serde_json::from_str(&encode_event(&event)).unwrap();
        assert!(
            matches!(decoded, ServerEvent::TurnState { latest_turn_id: 7, turn_id: active, .. } if active == turn_id)
        );
    }
}

#[test]
fn cancel_turn_scope_is_optional_and_roundtrips() {
    let legacy: Request = serde_json::from_str(r#"{"type":"cancel","id":3}"#).unwrap();
    assert!(matches!(legacy, Request::Cancel { id: 3, turn: None }));
    assert_eq!(
        serde_json::to_value(&legacy).unwrap(),
        serde_json::json!({"type":"cancel","id":3}),
        "an unscoped cancel keeps the legacy wire shape"
    );

    let scoped = Request::Cancel {
        id: 4,
        turn: Some(crate::TurnIdentity {
            runtime_generation: "runtime".into(),
            turn_id: 9,
        }),
    };
    let value = serde_json::to_value(&scoped).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "type": "cancel",
            "id": 4,
            "turn": {"runtime_generation": "runtime", "turn_id": 9}
        })
    );
    let decoded: Request = serde_json::from_value(value).unwrap();
    assert!(matches!(
        decoded,
        Request::Cancel { id: 4, turn: Some(identity) }
            if identity.runtime_generation == "runtime" && identity.turn_id == 9
    ));
}

fn identity(turn_id: u64) -> crate::TurnIdentity {
    crate::TurnIdentity {
        runtime_generation: "runtime".into(),
        turn_id,
    }
}

#[test]
fn turn_origin_events_roundtrip_with_origin_and_content() {
    let started = ServerEvent::TurnStarted {
        identity: identity(3),
        origin: Some(crate::TurnOrigin::client(Some("acp-1/2".into()), 41)),
        user_content: Some("what is the answer".into()),
    };
    let value = serde_json::to_value(&started).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "type": "turn_started",
            "identity": {"runtime_generation": "runtime", "turn_id": 3},
            "origin": {"kind": "client", "client_instance_id": "acp-1/2", "request_id": 41},
            "user_content": "what is the answer"
        })
    );
    let decoded: ServerEvent = serde_json::from_value(value).unwrap();
    let ServerEvent::TurnStarted {
        identity: decoded_identity,
        origin,
        user_content,
    } = decoded
    else {
        panic!("expected turn_started");
    };
    assert_eq!(decoded_identity, identity(3));
    assert_eq!(
        origin,
        Some(crate::TurnOrigin::client(Some("acp-1/2".into()), 41))
    );
    assert_eq!(user_content.as_deref(), Some("what is the answer"));

    for (status, wire) in [
        (crate::TurnEndStatus::Completed, "completed"),
        (crate::TurnEndStatus::Failed, "failed"),
        (crate::TurnEndStatus::Cancelled, "cancelled"),
    ] {
        let message = (status == crate::TurnEndStatus::Failed).then(|| "boom".to_string());
        let ended = ServerEvent::TurnEnded {
            identity: identity(3),
            status,
            message: message.clone(),
        };
        let value = serde_json::to_value(&ended).unwrap();
        assert_eq!(value["type"], "turn_ended");
        assert_eq!(value["status"], wire);
        assert_eq!(value.get("message").is_some(), message.is_some());
        let decoded: ServerEvent = serde_json::from_value(value).unwrap();
        assert!(matches!(
            decoded,
            ServerEvent::TurnEnded { identity: id, status: decoded_status, message: decoded_message }
                if id == identity(3) && decoded_status == status && decoded_message == message
        ));
    }
}

#[test]
fn turn_origin_decodes_every_kind_and_omits_absent_fields() {
    for (kind, wire) in [
        (crate::TurnOriginKind::Client, "client"),
        (crate::TurnOriginKind::Swarm, "swarm"),
        (crate::TurnOriginKind::Background, "background"),
        (crate::TurnOriginKind::Relay, "relay"),
        (crate::TurnOriginKind::Debug, "debug"),
    ] {
        let origin = crate::TurnOrigin::server(kind);
        let value = serde_json::to_value(&origin).unwrap();
        assert_eq!(value, serde_json::json!({"kind": wire}));
        assert_eq!(
            serde_json::from_value::<crate::TurnOrigin>(value).unwrap(),
            origin
        );
    }
}

#[test]
fn turn_origin_fields_are_backward_compatible() {
    // A pre-origin daemon's events still decode, with no origin or content.
    let started: ServerEvent = serde_json::from_str(
        r#"{"type":"turn_started","identity":{"runtime_generation":"runtime","turn_id":3}}"#,
    )
    .unwrap();
    assert!(matches!(
        started,
        ServerEvent::TurnStarted {
            origin: None,
            user_content: None,
            ..
        }
    ));
    let state: ServerEvent = serde_json::from_str(
        r#"{"type":"turn_state","session_id":"s","runtime_generation":"runtime","turn_id":3,"latest_turn_id":3}"#,
    )
    .unwrap();
    assert!(matches!(
        state,
        ServerEvent::TurnState {
            turn_id: Some(3),
            active_origin: None,
            active_user_content: None,
            ..
        }
    ));

    // An unattributed event keeps the pre-origin wire shape.
    let bare = ServerEvent::TurnStarted {
        identity: identity(3),
        origin: None,
        user_content: None,
    };
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        serde_json::json!({
            "type": "turn_started",
            "identity": {"runtime_generation": "runtime", "turn_id": 3}
        })
    );

    // A running turn's attribution travels with the attach boundary.
    let attributed = ServerEvent::TurnState {
        session_id: "s".into(),
        runtime_generation: "runtime".into(),
        turn_id: Some(3),
        latest_turn_id: 3,
        active_origin: Some(crate::TurnOrigin::server(crate::TurnOriginKind::Swarm)),
        active_user_content: Some("wake up".into()),
    };
    let decoded: ServerEvent = serde_json::from_str(&encode_event(&attributed)).unwrap();
    assert!(matches!(
        decoded,
        ServerEvent::TurnState {
            active_origin: Some(crate::TurnOrigin { kind: crate::TurnOriginKind::Swarm, .. }),
            active_user_content: Some(content),
            ..
        } if content == "wake up"
    ));
}

#[test]
fn turn_origin_capability_is_advertised() {
    let capabilities = crate::server_capabilities();
    assert!(
        capabilities
            .iter()
            .any(|c| c == crate::SERVER_CAPABILITY_TURN_ORIGIN)
    );
    assert_eq!(crate::SERVER_CAPABILITY_TURN_ORIGIN, "turn_origin");
    assert!(
        !capabilities.iter().any(|c| c == "turn_lifecycle"),
        "the fork's incompatible lifecycle capability must not be advertised"
    );
}
