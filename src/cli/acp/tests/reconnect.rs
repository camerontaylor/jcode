use super::*;

#[tokio::test]
async fn reconnect_after_eof_keeps_session_and_sends_next_prompt_once() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("private.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (client, old_daemon) = tokio::net::UnixStream::pair().unwrap();
    let (reader, writer) = client.into_split();
    let mut session = DaemonSession::new(
        "existing",
        Some(temp.path().into()),
        writer,
        "owner".into(),
        100,
    );
    session.socket_path = Some(socket);
    let session = Arc::new(session);
    *session.active_skill.lock().await = Some("keep".into());
    let runtime = AcpRuntime::new(AcpProfile::Standard, ProviderChoice::Claude, None, None);
    let (prompt_seen, prompt_received) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let Request::Ping { id } = serde_json::from_str(&line).unwrap() else {
            panic!("probe only")
        };
        writer
            .write_all(
                format!(
                    "{}\n",
                    json!({"type":"pong","id":id,"capabilities":["turn_origin"]})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let Request::Subscribe {
            id,
            target_session_id,
            allow_session_takeover,
            client_instance_id,
            ..
        } = serde_json::from_str(&line).unwrap()
        else {
            panic!("subscription only")
        };
        assert_eq!(target_session_id.as_deref(), Some("existing"));
        assert_eq!(client_instance_id.as_deref(), Some("owner"));
        assert!(!allow_session_takeover);
        for event in [
            json!({"type":"history","id":id,"session_id":"existing","messages":[],"all_sessions":["existing"]}),
            json!({"type":"done","id":id}),
        ] {
            writer
                .write_all(format!("{event}\n").as_bytes())
                .await
                .unwrap();
        }
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let Request::Message { content, .. } = serde_json::from_str(&line).unwrap() else {
            panic!("next prompt")
        };
        assert_eq!(content, "next prompt");
        prompt_seen.send(()).unwrap();
        // Keep the replacement alive while the caller verifies its view.
        std::future::pending::<()>().await;
    });
    // A request already admitted on the old transport must fail, not resend.
    let mut uncertain = session.register(99, PendingKind::Prompt).await;
    session.spawn_reader(reader, runtime);
    drop(old_daemon);
    tokio::time::timeout(std::time::Duration::from_secs(2), uncertain.recv())
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while session.disconnected.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(session.session_id().await, "existing");
    assert_eq!(session.active_skill.lock().await.as_deref(), Some("keep"));
    session
        .send(&Request::Message {
            id: session.next_id(),
            content: "next prompt".into(),
            images: vec![],
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        })
        .await
        .unwrap();
    // Abort only this test's mock daemon; no shared process is touched.
    tokio::time::timeout(std::time::Duration::from_secs(2), prompt_received)
        .await
        .unwrap()
        .unwrap();
    daemon.abort();
}

#[tokio::test]
async fn reconnect_unavailable_daemon_is_bounded_and_detach_stops_handshake() {
    for detach in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("unavailable.sock");
        let listener = if detach {
            Some(tokio::net::UnixListener::bind(&socket).unwrap())
        } else {
            None
        };
        let (client, old_daemon) = tokio::net::UnixStream::pair().unwrap();
        let (reader, writer) = client.into_split();
        let mut session = DaemonSession::new(
            "existing",
            Some(temp.path().into()),
            writer,
            "owner".into(),
            100,
        );
        session.socket_path = Some(socket);
        let session = Arc::new(session);
        let runtime = AcpRuntime::new(AcpProfile::Standard, ProviderChoice::Claude, None, None);
        session.spawn_reader(reader, runtime);
        drop(old_daemon);
        let _stalled_transport = if let Some(listener) = listener {
            let stream = tokio::time::timeout(std::time::Duration::from_secs(2), listener.accept())
                .await
                .unwrap()
                .unwrap()
                .0;
            session.router.lock().await.detach_view();
            Some(stream)
        } else {
            None
        };
        let deadline = if detach { 1 } else { 12 };
        tokio::time::timeout(std::time::Duration::from_secs(deadline), async {
            loop {
                if session
                    .reader_task
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .is_finished()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(session.disconnected.load(Ordering::SeqCst));
        assert!(session.send(&Request::Ping { id: 200 }).await.is_err());
    }
}

#[tokio::test]
async fn reconnect_captures_original_socket_and_rejects_missing_target() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("original.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (disconnect, disconnected) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(async move {
        let (initial, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = initial.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let Request::Ping { id } = serde_json::from_str(&line).unwrap() else {
            panic!("initial probe")
        };
        writer
            .write_all(
                format!(
                    "{}\n",
                    json!({"type":"pong","id":id,"capabilities":["turn_origin"]})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        disconnected.await.unwrap();
        drop(writer);
        drop(reader);
        let (replacement, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = replacement.into_split();
        let mut reader = BufReader::new(reader);
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let Request::Ping { id } = serde_json::from_str(&line).unwrap() else {
            panic!("replacement probe")
        };
        writer
            .write_all(
                format!(
                    "{}\n",
                    json!({"type":"pong","id":id,"capabilities":["turn_origin"]})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let Request::Subscribe {
            id,
            working_dir,
            target_session_id,
            ..
        } = serde_json::from_str(&line).unwrap()
        else {
            panic!("replacement subscribe")
        };
        assert_eq!(target_session_id.as_deref(), Some("missing"));
        assert!(
            working_dir.is_none(),
            "cwd would enable a new-session fallback"
        );
        writer
            .write_all(
                format!(
                    "{}\n",
                    json!({"type":"error","id":id,"message":"Unknown session"})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let (reader, writer) = stream.into_split();
    let runtime = AcpRuntime::new(AcpProfile::Standard, ProviderChoice::Claude, None, None);
    let session = runtime
        .start_daemon_session("missing", Some(temp.path().into()), reader, writer)
        .await
        .unwrap();
    assert_eq!(session.socket_path.as_ref(), Some(&socket));
    disconnect.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), daemon)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !session
            .reader_task
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_finished()
        {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(session.session_id().await, "missing");
    assert!(session.disconnected.load(Ordering::SeqCst));
}
