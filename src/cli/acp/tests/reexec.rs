//! Opt-in process proof against an explicitly supplied daemon binary. All
//! sockets, configuration, provider traffic and reload state are private.
use super::*;
use tokio::io::AsyncReadExt;

async fn server_identity(socket: &Path) -> Result<String> {
    let mut client = crate::server::Client::connect_debug_with_path(socket.into()).await?;
    let id = client.debug_command("server:info", None).await?;
    loop {
        if let ServerEvent::DebugResponse {
            id: reply,
            ok,
            output,
        } = client.read_event().await?
            && reply == id
        {
            anyhow::ensure!(ok, "{output}");
            return Ok(serde_json::from_str::<Value>(&output)?["id"]
                .as_str()
                .unwrap()
                .into());
        }
    }
}

async fn fake_prompt_once(session: &Arc<DaemonSession>) -> Result<String> {
    let id = session.next_id();
    let mut rx = session.register(id, PendingKind::Prompt).await;
    session
        .send(&Request::Message {
            id,
            content: "reply once".into(),
            images: vec![],
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        })
        .await?;
    let mut text = String::new();
    loop {
        match next_routed(&mut rx).await? {
            ServerEvent::TextDelta { text: delta } => text.push_str(&delta),
            ServerEvent::Done { id: reply } if reply == id => break,
            ServerEvent::Error { message, .. } => anyhow::bail!("{message}"),
            _ => {}
        }
    }
    session.unregister(id).await;
    Ok(text)
}

#[tokio::test]
#[ignore = "requires JCODE_ACP_REEXEC_TEST_BINARY pointing to an existing daemon executable"]
async fn reconnect_real_private_daemon_reexec_then_fake_provider_prompt() -> Result<()> {
    let binary = std::env::var("JCODE_ACP_REEXEC_TEST_BINARY")?;
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let runtime_dir = temp.path().join("runtime");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&runtime_dir)?;
    let socket = runtime_dir.join("jcode.sock");
    let debug_socket = runtime_dir.join("jcode-debug.sock");
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = http.local_addr()?;
    std::fs::write(
        home.join("config.toml"),
        format!(
            r#"
[provider]
default = "reconnect"
[providers.reconnect]
type = "openai-compatible"
base_url = "http://{address}/v1"
auth = "none"
requires_api_key = false
default_model = "test-model"
model_catalog = true
"#
        ),
    )?;
    let prompts = Arc::new(AtomicUsize::new(0));
    let http_prompts = prompts.clone();
    let provider = tokio::spawn(async move {
        loop {
            let (stream, _) = http.accept().await.unwrap();
            let prompts = http_prompts.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap() == 0 {
                    return;
                }
                let post = line.starts_with("POST ");
                let mut length = 0;
                loop {
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap() == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await.unwrap();
                let (kind, body) = if post {
                    prompts.fetch_add(1, Ordering::SeqCst);
                    (
                        "text/event-stream",
                        concat!(
                            "data: {\"id\":\"fake\",\"object\":\"chat.completion.chunk\",\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"private-reexec-ok\"},\"finish_reason\":null}]}\n\n",
                            "data: {\"id\":\"fake\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                            "data: [DONE]\n\n"
                        ),
                    )
                } else {
                    (
                        "application/json",
                        r#"{"data":[{"id":"test-model","name":"Test","context_length":32000}]}"#,
                    )
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            });
        }
    });
    let stderr = std::fs::File::create(temp.path().join("daemon-stderr.log"))?;
    let mut daemon = tokio::process::Command::new(binary)
        .args([
            "--no-update",
            "--provider",
            "openai-compatible",
            "--model",
            "test-model",
            "serve",
            "--socket",
        ])
        .arg(&socket)
        .env("JCODE_HOME", &home)
        .env("JCODE_RUNTIME_DIR", &runtime_dir)
        .env("JCODE_SOCKET", &socket)
        .env("JCODE_INSTALL_DIR", temp.path().join("install"))
        .env("JCODE_PROVIDER_PROFILE_NAME", "reconnect")
        .env("JCODE_DEBUG_CONTROL", "1")
        .env("JCODE_NO_TELEMETRY", "1")
        .env("JCODE_MEMORY_ENABLED", "0")
        .env("JCODE_MEMORY_SIDECAR_ENABLED", "0")
        .env("JCODE_TEMP_SERVER", "1")
        .env("JCODE_SERVER_OWNER_PID", std::process::id().to_string())
        .env_remove("JCODE_TEST_SESSION")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr)
        .kill_on_drop(true)
        .spawn()?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(45), async {
        let stream = loop {
            match tokio::net::UnixStream::connect(&socket).await {
                Ok(stream) => break stream,
                Err(err) => {
                    anyhow::ensure!(daemon.try_wait()?.is_none(), "Daemon exited before startup: {err}");
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            }
        };
        let (reader, writer) = stream.into_split();
        let runtime = AcpRuntime::new(AcpProfile::Standard, ProviderChoice::OpenaiCompatible, None, None);
        let session = runtime.create_new_session_connected(temp.path().into(), reader, writer).await?;
        let native_id = session.session_id().await;
        // An entirely empty session has no transcript to restore after exec.
        // Establish a persisted native session with one deterministic turn.
        anyhow::ensure!(fake_prompt_once(&session).await? == "private-reexec-ok", "Seed prompt failed");
        let before = server_identity(&debug_socket).await?;
        let mut lost = session.register(session.next_id(), PendingKind::Reply).await;
        session.send(&Request::Reload { id: session.next_id(), force: true }).await?;
        while lost.recv().await.is_some() {}
        while session.disconnected.load(Ordering::SeqCst) {
            anyhow::ensure!(!session.reader_task.lock().unwrap().as_ref().unwrap().is_finished(), "Recovery exited");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let after = server_identity(&debug_socket).await?;
        anyhow::ensure!(before != after, "Daemon identity did not change across exec");
        anyhow::ensure!(daemon.try_wait()?.is_none(), "Original daemon process exited instead of execing");
        anyhow::ensure!(session.session_id().await == native_id, "Session changed");
        let text = fake_prompt_once(&session).await?;
        anyhow::ensure!(text == "private-reexec-ok", "Unexpected output: {text:?}");
        anyhow::ensure!(prompts.load(Ordering::SeqCst) == 2, "Provider prompt was duplicated");
        eprintln!("private reexec proof: pid={:?} server={before}->{after} native_session={native_id} provider_calls=2 (seed + next) output={text}", daemon.id());
        Ok::<_, anyhow::Error>(())
    }).await;
    if daemon.try_wait()?.is_none() {
        daemon.kill().await?;
    }
    provider.abort();
    if !matches!(&result, Ok(Ok(()))) {
        eprintln!(
            "private daemon stderr: {}",
            std::fs::read_to_string(temp.path().join("daemon-stderr.log"))?
        );
        eprintln!("private evidence retained at {}", temp.keep().display());
    }
    result??;
    Ok(())
}
