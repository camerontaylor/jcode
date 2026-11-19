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

fn owned_manager() -> tokio::sync::RwLock<crate::mcp::McpManager> {
    tokio::sync::RwLock::new(crate::mcp::McpManager::with_config(
        crate::mcp::McpConfig::default(),
    ))
}

/// The first turn waits for an initial handshake that finishes within the
/// grace window, so a fast server's instructions are not lost to the race.
#[tokio::test]
async fn first_turn_waits_for_a_handshake_within_the_grace() {
    let _guard = crate::storage::lock_test_env();
    let capture = SystemCaptureProvider::default();
    let systems = capture.systems.clone();
    let provider: Arc<dyn Provider> = Arc::new(capture);
    let registry = Registry::new(provider.clone()).await;
    registry.begin_mcp_connects(1);
    let background = registry.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        background.set_mcp_server_instructions("fast", Some("Fast guidance.".into()));
        // Settle the pending handshake (the outcome itself is not under test).
        let manager = owned_manager();
        background
            .finish_mcp_connect(&manager, "other", Err(anyhow::anyhow!("done")))
            .await;
    });
    let mut agent = Agent::new(provider, registry);

    run_turn(&mut agent, "first").await;

    assert!(systems.lock().unwrap()[0].contains("## fast\nFast guidance."));
}

/// A handshake that never finishes delays the first turn by at most the
/// grace, and anything it sends afterwards is left out.
#[tokio::test]
async fn hung_handshake_delays_the_first_turn_by_at_most_the_grace() {
    let _guard = crate::storage::lock_test_env();
    let capture = SystemCaptureProvider::default();
    let systems = capture.systems.clone();
    let provider: Arc<dyn Provider> = Arc::new(capture);
    let registry = Registry::new(provider.clone()).await;
    registry.begin_mcp_connects(1);
    let mut agent = Agent::new(provider, registry.clone());

    let started = std::time::Instant::now();
    run_turn(&mut agent, "first").await;
    let elapsed = started.elapsed();
    registry.set_mcp_server_instructions("hung", Some("Too late.".into()));
    run_turn(&mut agent, "second").await;

    let grace = crate::tool::mcp::instructions::MCP_INSTRUCTIONS_GRACE;
    assert!(
        elapsed < grace + std::time::Duration::from_secs(2),
        "{elapsed:?}"
    );
    let systems = systems.lock().unwrap();
    assert!(systems.iter().all(|system| !system.contains("Too late.")));
}
