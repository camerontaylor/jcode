use super::*;

/// QA-STOP: the 500 ms peer reset races bash's 750 ms shutdown handoff.
/// Real bash runs until a file is released. File readiness, rather than a
/// delay, establishes that Stop lands inside the tool wait. The bounded
/// terminal deadline spans the handoff window; cleanup releases the tool even
/// on the known failing baseline. No production implementation is patched here.
#[tokio::test]
async fn acp_conformance_stop_observed_bash_wait() -> Result<()> {
    let _env = setup_test_env()?;
    let _tools = EnvVarGuard::set("JCODE_TOOLS", "bash");
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let ready = runtime.path().join("ready");
    let release = runtime.path().join("release");
    let provider = Arc::new(MockProvider::new());
    // Paths are generated under a fixed-prefix temporary directory, with no
    // user-controlled shell text. This is a waiting tool, not a timed response.
    let command = format!(
        "printf ready > '{}'; while [ ! -e '{}' ]; do sleep 0.05; done",
        ready.display(),
        release.display()
    );
    provider.queue_response(vec![
        StreamEvent::ToolUseStart {
            id: "held-bash".into(),
            name: "bash".into(),
        },
        StreamEvent::ToolInputDelta(json!({"command":command}).to_string()),
        StreamEvent::ToolUseEnd,
        StreamEvent::MessageEnd {
            stop_reason: Some("tool_use".into()),
        },
    ]);
    provider.queue_response(vec![
        StreamEvent::TextDelta("must-not-continue".into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]);
    let daemon =
        server::Server::new_with_paths(provider, socket.clone(), runtime.path().join("debug.sock"));
    let task = tokio::spawn(async move { daemon.run().await });
    let result = async {
        wait_for_socket(&socket).await?;
        let mut raw = wait_for_subscribed_server_client(&socket).await?;
        let session = match raw.get_history_event().await? {
            ServerEvent::History { session_id, .. } => session_id,
            event => anyhow::bail!("expected history: {event:?}"),
        };
        let mut acp = AcpClient::spawn()?;
        initialize(&mut acp).await?;
        let attachment = acp
            .request(
                "session/resume",
                json!({"sessionId":session,"cwd":std::env::current_dir()?}),
            )
            .await?;
        assert_eq!(
            attachment["_meta"]["jcode"]["sharedControl"]["stopObservedTurn"],
            true
        );
        raw.send_message("hold a tool").await?;
        let start = acp
            .wait_for_notification(|m| {
                m["method"] == "_jcode/turn" && m["params"]["state"] == "started"
            })
            .await?;
        timeout(Duration::from_secs(10), async {
            while !ready.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("bash reached file gate")?;
        acp.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}))
            .await?;
        let end = timeout(
            Duration::from_secs(3),
            acp.wait_for_notification(|m| {
                m["method"] == "_jcode/turn" && m["params"]["state"] != "started"
            }),
        )
        .await
        .context("Stop must retire the observed tool-wait turn")??;
        assert_eq!(end["params"]["state"], "canceled");
        assert_eq!(end["params"]["turnId"], start["params"]["turnId"]);
        assert!(
            !acp.tape
                .iter()
                .any(|r| r["msg"]["params"]["update"]["content"]["text"] == "must-not-continue")
        );
        acp.shutdown().await;
        Ok(())
    }
    .await;
    std::fs::write(release, b"release")?;
    task.abort();
    result
}
