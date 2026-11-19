use super::*;

#[derive(Clone, Default)]
struct SystemCaptureProvider {
    systems: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for SystemCaptureProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.systems.lock().unwrap().push(system.to_string());
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(4);
        tokio::spawn(async move {
            let _ = tx.send(Ok(StreamEvent::TextDelta("ok".to_string()))).await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

async fn run_turn(agent: &mut Agent, text: &str) {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc(text, Vec::new(), None, tx)
        .await
        .expect("turn");
}

/// Instructions from servers connected before the first turn are rendered in
/// the system prompt, each under its server name; a server connecting later
/// changes nothing, so the cached prefix stays stable.
#[tokio::test]
async fn mcp_instructions_are_frozen_at_the_first_turn() {
    let _guard = crate::storage::lock_test_env();
    let capture = SystemCaptureProvider::default();
    let systems = capture.systems.clone();
    let provider: Arc<dyn Provider> = Arc::new(capture);
    let registry = Registry::new(provider.clone()).await;
    registry.set_mcp_server_instructions("zeta", Some("Zeta guidance.".into()));
    registry.set_mcp_server_instructions("alpha", Some("Alpha guidance.".into()));
    let mut agent = Agent::new(provider, registry.clone());

    run_turn(&mut agent, "first").await;
    registry.set_mcp_server_instructions("late", Some("Late guidance.".into()));
    run_turn(&mut agent, "second").await;

    let systems = systems.lock().unwrap();
    assert_eq!(systems.len(), 2);
    assert_eq!(
        systems[0], systems[1],
        "late connections must not change it"
    );
    let system = &systems[0];
    let section = system
        .find("# MCP Server Instructions")
        .expect("MCP section present");
    let alpha = system.find("## alpha\nAlpha guidance.").expect("alpha");
    let zeta = system.find("## zeta\nZeta guidance.").expect("zeta");
    assert!(section < alpha && alpha < zeta);
    assert!(!system.contains("Late guidance."));
}

#[tokio::test]
async fn no_mcp_instructions_adds_no_section() {
    let _guard = crate::storage::lock_test_env();
    let capture = SystemCaptureProvider::default();
    let systems = capture.systems.clone();
    let provider: Arc<dyn Provider> = Arc::new(capture);
    let registry = Registry::new(provider.clone()).await;
    registry.set_mcp_server_instructions("gone", Some("Gone.".into()));
    registry.set_mcp_server_instructions("gone", None);
    let mut agent = Agent::new(provider, registry);

    run_turn(&mut agent, "first").await;

    let systems = systems.lock().unwrap();
    assert!(!systems[0].contains("MCP Server Instructions"));
}
