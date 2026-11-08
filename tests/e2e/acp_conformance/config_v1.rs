use super::*;

#[tokio::test]
async fn acp_conformance_config_commands_v1() -> Result<()> {
    config_scenario(false).await
}

#[tokio::test]
async fn acp_conformance_config_peer_update_contract() -> Result<()> {
    config_scenario(true).await
}

async fn config_scenario(check_peer: bool) -> Result<()> {
    let _env = setup_test_env()?;
    let runtime = tempfile::tempdir_in("/tmp")?;
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_DIR", runtime.path());
    let socket = runtime.path().join("jcode.sock");
    let server = server::Server::new_with_paths(
        Arc::new(ConfigProvider {
            inner: MockProvider::with_models(vec!["mock-initial", "mock-future-catalog"]),
            effort: std::sync::Mutex::new("medium".into()),
        }),
        socket.clone(),
        runtime.path().join("debug.sock"),
    );
    let task = tokio::spawn(async move { server.run().await });
    let result = async {
        wait_for_socket(&socket).await?;
        let mut a = AcpClient::spawn()?;
        let mut b = AcpClient::spawn()?;
        initialize(&mut a).await?;
        initialize(&mut b).await?;
        let cwd = std::env::current_dir()?;
        let new = a.request("session/new", json!({"cwd":cwd})).await?;
        let id = &new["sessionId"];
        b.request("session/resume", json!({"sessionId":id,"cwd":cwd}))
            .await?;
        let commands = a
            .wait_for_notification(|m| {
                m["params"]["update"]["sessionUpdate"] == "available_commands_update"
            })
            .await?;
        let names: Vec<_> = commands["params"]["update"]["availableCommands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        for name in ["model", "models", "effort", "compact", "subagent"] {
            assert!(names.contains(&name));
        }
        let model = new["models"]["availableModels"][1]["modelId"].clone();
        assert!(model.is_string());
        for (method, extra) in [
            (
                "session/set_config_option",
                json!({"configId":"model","value":model}),
            ),
            ("session/set_model", json!({"modelId":model})),
            ("session/set_model", json!({"model":model})),
            (
                "session/set_config_option",
                json!({"configId":"reasoning_effort","value":"high"}),
            ),
            ("session/set_reasoning_effort", json!({"effort":"high"})),
        ] {
            let mut params = extra;
            params["sessionId"] = id.clone();
            let response = a.request(method, params.clone()).await?;
            let options = response["configOptions"].as_array().unwrap();
            if params["configId"] == "reasoning_effort" || method == "session/set_reasoning_effort"
            {
                assert!(
                    options
                        .iter()
                        .any(|o| o["id"] == "reasoning_effort" && o["currentValue"] == "high")
                );
            }
            assert!(
                options
                    .iter()
                    .any(|o| o["id"] == "model" && o["currentValue"] == model)
            );
            assert!(
                options
                    .iter()
                    .all(|o| o["id"].is_string() && o["options"].is_array())
            );
            a.wait_for_notification(|m| {
                m["params"]["update"]["sessionUpdate"] == "config_option_update"
            })
            .await?;
            if check_peer && method == "session/set_config_option" && params["configId"] == "model"
            {
                let update = b
                    .wait_for_notification(|m| {
                        m["params"]["update"]["sessionUpdate"] == "config_option_update"
                    })
                    .await?;
                assert_eq!(
                    update["params"]["update"]["configOptions"],
                    response["configOptions"]
                );
                assert!(
                    update["params"]["update"]["configOptions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|o| o["id"] == "model" && o["currentValue"] == model)
                );
            }
        }
        let selected = a
            .request(
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"/model"}]}),
            )
            .await?;
        assert_eq!(
            selected["_meta"]["jcode"],
            json!({"turnId":null,"role":"none"})
        );
        acp_fixture::check("07-config-commands-v1.jsonl", &a.tape)?;
        a.shutdown().await;
        b.shutdown().await;
        Ok(())
    }
    .await;
    task.abort();
    result
}

/// A catalog and effort-capable mock; all behavior stays inside the test.
struct ConfigProvider {
    inner: MockProvider,
    effort: std::sync::Mutex<String>,
}
#[async_trait]
impl Provider for ConfigProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        session: Option<&str>,
    ) -> Result<EventStream> {
        self.inner.complete(messages, tools, system, session).await
    }
    fn name(&self) -> &str {
        "mock"
    }
    fn model(&self) -> String {
        self.inner.model()
    }
    fn set_model(&self, model: &str) -> Result<()> {
        self.inner.set_model(model)
    }
    fn available_models(&self) -> Vec<&'static str> {
        self.inner.available_models()
    }
    fn reasoning_effort(&self) -> Option<String> {
        Some(self.effort.lock().unwrap().clone())
    }
    fn set_reasoning_effort(&self, effort: &str) -> Result<()> {
        *self.effort.lock().unwrap() = effort.into();
        Ok(())
    }
    fn available_efforts(&self) -> Vec<&'static str> {
        vec!["low", "medium", "high"]
    }
    fn fork(&self) -> Arc<dyn Provider> {
        let inner = MockProvider::with_models(self.inner.available_models());
        inner.set_model(&self.inner.model()).unwrap();
        Arc::new(Self {
            inner,
            effort: std::sync::Mutex::new(self.effort.lock().unwrap().clone()),
        })
    }
}
