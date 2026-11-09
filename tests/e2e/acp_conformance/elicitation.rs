use super::*;

fn queue_form(provider: &MockProvider) {
    provider.queue_response(vec![StreamEvent::ToolUseStart {id:"form-call".into(),name:"applet".into()},
        StreamEvent::ToolInputDelta(json!({"instance":"survey","title":"Survey","wait":true,"view":{"type":"stack","children":[
            {"type":"input","bind":"q","placeholder":"Your answer"},
            {"type":"button","label":"Save","on_press":{"action":"save"}}
        ]},"state":{"q":"initial","kept":true}}).to_string()),
        StreamEvent::ToolUseEnd,StreamEvent::MessageEnd {stop_reason:Some("tool_use".into())}]);
    provider.queue_response(vec![
        StreamEvent::TextDelta("form-finished".into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]);
}
async fn initialize_form(client: &mut AcpClient) -> Result<()> {
    client
        .request(
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{"elicitation":{"form":{}}}}),
        )
        .await?;
    Ok(())
}
async fn start_form(client: &mut AcpClient, session: &str) -> Result<(u64, Value)> {
    let prompt = client
        .send_request(
            "session/prompt",
            json!({"sessionId":session,"prompt":[{"type":"text","text":"Ask a form"}]}),
        )
        .await?;
    let form = client
        .wait_for_notification(|m| m["method"] == "elicitation/create")
        .await?;
    Ok((prompt, form))
}

#[tokio::test]
async fn acp_conformance_elicitation_form_v1() -> Result<()> {
    let _env = setup_test_env()?;
    let _tools = EnvVarGuard::set("JCODE_TOOLS", "applet");
    jcode::config::invalidate_config_cache();
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let provider = Arc::new(MockProvider::new());
    queue_form(&provider);
    let daemon = server::Server::new_with_paths(
        provider.clone(),
        socket.clone(),
        runtime.path().join("debug.sock"),
    );
    let task = tokio::spawn(async move { daemon.run().await });
    let result=async {
        wait_for_socket(&socket).await?;
        let mut c=AcpClient::spawn()?;initialize_form(&mut c).await?;
        let created=c.request("session/new",json!({"cwd":std::env::current_dir()?})).await?;
        assert_eq!(created["_meta"]["jcode"]["appletElicitation"],true);
        let session=created["sessionId"].as_str().context("session ID")?.to_string();
        let (prompt,form)=start_form(&mut c,&session).await?;
        assert_eq!(form["params"]["mode"],"form");assert_eq!(form["params"]["toolCallId"],"form-call");
        assert_eq!(form["params"]["requestedSchema"]["properties"]["q"]["default"],"initial");
        c.send(json!({"jsonrpc":"2.0","id":form["id"],"result":{"action":"accept","content":{"q":7}}})).await?;
        let retry=c.wait_for_notification(|m|m["method"]=="elicitation/create").await?;
        assert_ne!(form["id"],retry["id"]);
        c.send(json!({"jsonrpc":"2.0","id":retry["id"],"result":{"action":"accept","content":{"q":"answer"},"_meta":{"consumer":"test"}}})).await?;
        assert!(c.wait_for_response(prompt).await?["result"].is_object());
        let stored=jcode::applets::snapshot_for_session(&session)?;
        assert_eq!(stored.get("survey").unwrap().document.state,json!({"q":"answer","kept":true}));
        let history=serde_json::to_string(&Session::load(&session)?.messages)?;
        assert!(history.contains("[applet action]")&&history.contains("save"));
        acp_fixture::check("09-elicitation-form-v1.jsonl",&c.tape)?;
        c.shutdown().await;
        for action in ["decline","cancel"] {
            queue_form(&provider);
            let mut c=AcpClient::spawn()?;initialize_form(&mut c).await?;
            c.request("session/resume",json!({"sessionId":session,"cwd":std::env::current_dir()?})).await?;
            let (prompt,form)=start_form(&mut c,&session).await?;
            c.send(json!({"jsonrpc":"2.0","id":form["id"],"result":{"action":action,"content":null,"_meta":{}}})).await?;
            c.wait_for_response(prompt).await?;
            let history=serde_json::to_string(&Session::load(&session)?.messages)?;
            assert!(history.contains(if action=="decline" {"host.declined"}else{"host.cancelled"}));
            c.shutdown().await;
        }
        Ok::<(),anyhow::Error>(())
    }.await;
    task.abort();
    result
}

#[tokio::test]
async fn acp_conformance_elicitation_peer_winner_and_turn_cancel() -> Result<()> {
    let _env = setup_test_env()?;
    let _tools = EnvVarGuard::set("JCODE_TOOLS", "applet");
    jcode::config::invalidate_config_cache();
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let provider = Arc::new(MockProvider::new());
    queue_form(&provider);
    let daemon = server::Server::new_with_paths(
        provider.clone(),
        socket.clone(),
        runtime.path().join("debug.sock"),
    );
    let task = tokio::spawn(async move { daemon.run().await });
    let result=async {
        wait_for_socket(&socket).await?;
        let mut a=AcpClient::spawn()?;initialize_form(&mut a).await?;
        let created=a.request("session/new",json!({"cwd":std::env::current_dir()?})).await?;
        let session=created["sessionId"].as_str().context("session ID")?.to_string();
        let (prompt,form_a)=start_form(&mut a,&session).await?;
        let mut b=AcpClient::spawn()?;initialize_form(&mut b).await?;
        b.request("session/resume",json!({"sessionId":session,"cwd":std::env::current_dir()?})).await?;
        let form_b=b.wait_for_notification(|m|m["method"]=="elicitation/create").await?;
        b.send(json!({"jsonrpc":"2.0","id":form_b["id"],"result":{"action":"accept","content":{"q":"winner"}}})).await?;
        let cancel=a.wait_for_notification(|m|m["method"]=="$/cancel_request").await?;
        assert_eq!(cancel["params"]["requestId"],form_a["id"]);
        a.send(json!({"jsonrpc":"2.0","id":form_a["id"],"result":{"action":"accept","content":{"q":"late"}}})).await?;
        a.wait_for_response(prompt).await?;
        assert!(!a.tape.iter().any(|line|line["dir"]=="a2c" && line["msg"]["id"]==form_a["id"] && line["msg"].get("error").is_some()));
        assert_eq!(jcode::applets::snapshot_for_session(&session)?.get("survey").unwrap().document.state["q"],"winner");
        b.shutdown().await;
        #[cfg(unix)]
        {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let stream=tokio::net::UnixStream::connect(&socket).await?;
            let (reader,mut writer)=stream.into_split();let mut reader=BufReader::new(reader);
            writer.write_all(format!("{}\n{}\n",json!({"type":"subscribe","id":800,"target_session_id":session,"supports_user_response":true}),json!({"type":"get_history","id":801})).as_bytes()).await?;
            loop {
                let mut line=String::new();reader.read_line(&mut line).await?;
                let event:Value=serde_json::from_str(&line)?;
                if event["type"]=="history" {break;}
            }
            queue_form(&provider);
            let (prompt,form)=start_form(&mut a,&session).await?;
            writer.write_all(format!("{}\n",json!({"type":"applet_action","id":802,"session_id":session,"instance":"survey","action":{"action":"save"},"state":{"q":"desktop","kept":true}})).as_bytes()).await?;
            let cancel=a.wait_for_notification(|m|m["method"]=="$/cancel_request").await?;
            assert_eq!(cancel["params"]["requestId"],form["id"]);
            a.wait_for_response(prompt).await?;
            assert_eq!(jcode::applets::snapshot_for_session(&session)?.get("survey").unwrap().document.state["q"],"desktop");
        }
        queue_form(&provider);
        let (prompt,form)=start_form(&mut a,&session).await?;
        a.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}})).await?;
        let cancel=timeout(Duration::from_secs(3),a.wait_for_notification(|m|m["method"]=="$/cancel_request")).await??;
        assert_eq!(cancel["params"]["requestId"],form["id"]);
        timeout(Duration::from_secs(3),a.wait_for_response(prompt)).await??;
        a.shutdown().await;Ok::<(),anyhow::Error>(())
    }.await;
    task.abort();
    result
}

#[tokio::test]
async fn acp_conformance_elicitation_requires_capability_and_opt_in() -> Result<()> {
    let _env = setup_test_env()?;
    for (tools, form_support) in [("applet", false), ("bash", true)] {
        let _tools = EnvVarGuard::set("JCODE_TOOLS", tools);
        jcode::config::invalidate_config_cache();
        let runtime = tempfile::tempdir_in("/tmp")?;
        let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
        let socket = runtime.path().join("jcode.sock");
        let provider = Arc::new(MockProvider::new());
        queue_form(&provider);
        let daemon = server::Server::new_with_paths(
            provider.clone(),
            socket.clone(),
            runtime.path().join("debug.sock"),
        );
        let task = tokio::spawn(async move { daemon.run().await });
        let result=async {
            wait_for_socket(&socket).await?;let mut client=AcpClient::spawn()?;
            if form_support {initialize_form(&mut client).await?;}else{initialize(&mut client).await?;}
            let created=client.request("session/new",json!({"cwd":std::env::current_dir()?})).await?;
            if form_support {assert_eq!(created["_meta"]["jcode"]["appletElicitation"],false);}else{assert!(created.pointer("/_meta/jcode/appletElicitation").is_none());}
            let session=created["sessionId"].as_str().context("session ID")?;
            let prompt=client.send_request("session/prompt",json!({"sessionId":session,"prompt":[{"type":"text","text":"Try unavailable form"}]})).await?;
            let response=client.wait_for_response(prompt).await?;
            assert!(response["result"].is_object() || response["error"].is_object());
            {
                let calls=provider.captured_tool_names.lock().unwrap();
                assert!(!calls.is_empty());
                assert!(calls.iter().all(|tools|!tools.iter().any(|tool|tool=="applet")));
            }
            assert!(!client.tape.iter().any(|line|line["msg"]["method"]=="elicitation/create"));
            client.shutdown().await;Ok::<(),anyhow::Error>(())
        }.await;
        task.abort();
        result?;
    }
    Ok(())
}
